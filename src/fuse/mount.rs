//! Mount helpers for starting/stopping FUSE
//!
//! Notes:
//! - Only supported on Unix-like systems. On Linux we support unprivileged mount via fusermount3
//!   and privileged mount via /dev/fuse.
//! - These helpers are thin wrappers over asyncfuse raw Session APIs.

use std::num::NonZeroU32;
use std::path::Path;

use asyncfuse::MountOptions;
#[cfg(target_os = "linux")]
use asyncfuse::raw::logfs::LoggingFileSystem;

use crate::chunk::store::BlockStore;
use crate::fuse::BREWFS_FUSE_MAX_WRITE;
use crate::meta::MetaLayer;
use crate::vfs::fs::VFS;

#[derive(Debug, Clone, Copy, Default)]
pub struct FuseConcurrencyConfig {
    pub worker_count: usize,
    pub max_background: usize,
}

/// Build default mount options for BrewFS.
fn default_mount_options() -> MountOptions {
    let mut mo = MountOptions::default();
    mo.fs_name("brewfs");
    mo.default_permissions(true);
    // Keep the kernel writeback-cache opt-in.  It can help mmap-heavy
    // single-client workloads, but Linux can zero already-written mmap bytes
    // around byte-granular fallocate extension (xfstests generic/438), so the
    // correctness default is the write-through FUSE page-cache path.
    mo.write_back(fuse_writeback_enabled());
    // Allow other users to access the filesystem (required for multi-user scenarios and xfstests)
    // Note: Requires 'user_allow_other' in /etc/fuse.conf for non-root mounts
    mo.allow_other(true);
    // Default to 4 MiB for higher throughput while keeping memory usage reasonable.
    mo.max_write(NonZeroU32::new(BREWFS_FUSE_MAX_WRITE).unwrap());
    mo.custom_options(format!("max_read={BREWFS_FUSE_MAX_WRITE}"));
    // Set kernel readahead to 16 MiB (4 blocks). Larger values cause excessive
    // concurrent FUSE reads that create scheduling contention. 16 MiB lets the
    // kernel pipeline 4 read requests while our userspace prefetcher handles
    // deeper look-ahead independently.
    mo.max_readahead(Some(16 * 1024 * 1024));
    mo
}

fn fuse_writeback_enabled() -> bool {
    parse_fuse_writeback_enabled(std::env::var("BREWFS_FUSE_WRITEBACK").ok())
}

fn parse_fuse_writeback_enabled(value: Option<String>) -> bool {
    value
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

fn configure_session<FS>(
    session: asyncfuse::raw::Session<FS>,
    config: FuseConcurrencyConfig,
) -> asyncfuse::raw::Session<FS>
where
    FS: asyncfuse::raw::Filesystem + Send + Sync + 'static,
{
    if config.worker_count > 1 {
        session.with_workers(config.worker_count, config.max_background.max(1))
    } else {
        session
    }
}

#[cfg(target_os = "linux")]
fn decode_mountinfo_path(value: &str) -> std::path::PathBuf {
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let mut octal = String::new();
            for _ in 0..3 {
                if let Some(digit) = chars.next() {
                    octal.push(digit);
                }
            }
            if octal.len() == 3
                && octal.as_bytes().iter().all(|digit| digit.is_ascii_digit())
                && let Ok(byte) = u8::from_str_radix(&octal, 8)
            {
                decoded.push(byte as char);
                continue;
            }
            decoded.push('\\');
            decoded.push_str(&octal);
        } else {
            decoded.push(ch);
        }
    }
    std::path::PathBuf::from(decoded)
}

#[cfg(target_os = "linux")]
fn current_mount_id(mount_point: &Path) -> std::io::Result<u64> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    for line in mountinfo.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let mut fields = left.split_whitespace();
        let Ok(mount_id) = fields.next().unwrap_or_default().parse::<u64>() else {
            continue;
        };
        let _parent_id = fields.next();
        let _major_minor = fields.next();
        let _root = fields.next();
        let Some(mountpoint) = fields.next().map(decode_mountinfo_path) else {
            continue;
        };
        let mut super_fields = right.split_whitespace();
        let Some(fs_type) = super_fields.next() else {
            continue;
        };
        let Some(source) = super_fields.next() else {
            continue;
        };
        if mountpoint == mount_point
            && matches!(fs_type, "fuse" | "fuse.brewfs")
            && source == "brewfs"
        {
            return Ok(mount_id);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "BrewFS mount is missing from /proc/self/mountinfo",
    ))
}

#[cfg(target_os = "linux")]
fn bind_mount_identity<S, M>(fs: &VFS<S, M>) -> std::io::Result<()>
where
    S: BlockStore + Send + Sync + 'static,
    M: MetaLayer + Send + Sync + 'static,
{
    let canonical = fs.fuse_mount_point().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "VFS mount point was not bound before mounting",
        )
    })?;
    fs.set_fuse_mount_id(current_mount_id(canonical)?)
}

#[cfg(target_os = "linux")]
fn fuse_op_log_enabled() -> bool {
    std::env::var("BREWFS_FUSE_OP_LOG")
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

/// Mount a VFS instance to the given empty directory using unprivileged mode when available.
#[cfg(target_os = "linux")]
pub async fn mount_vfs_unprivileged<S, M>(
    fs: VFS<S, M>,
    mount_point: impl AsRef<Path>,
    concurrency: FuseConcurrencyConfig,
) -> std::io::Result<asyncfuse::raw::MountHandle>
where
    S: BlockStore + Send + Sync + 'static,
    M: MetaLayer + Send + Sync + 'static,
{
    let mount_point = mount_point.as_ref();
    fs.set_fuse_mount_point(mount_point)?;
    let identity = fs.clone();
    // Prefer unprivileged mount on Linux (requires fusermount3 in PATH)
    if fuse_op_log_enabled() {
        configure_session(
            asyncfuse::raw::Session::new(default_mount_options()),
            concurrency,
        )
        .mount_with_unprivileged(LoggingFileSystem::new(fs), mount_point)
        .await
        .and_then(|handle| {
            bind_mount_identity(&identity)?;
            Ok(handle)
        })
    } else {
        configure_session(
            asyncfuse::raw::Session::new(default_mount_options()),
            concurrency,
        )
        .mount_with_unprivileged(fs, mount_point)
        .await
        .and_then(|handle| {
            bind_mount_identity(&identity)?;
            Ok(handle)
        })
    }
}

/// Mount a VFS instance to the given empty directory using privileged mode (via /dev/fuse).
/// Requires root or fuse group membership. Supports allow_other without /etc/fuse.conf tweaks.
#[cfg(target_os = "linux")]
pub async fn mount_vfs_privileged<S, M>(
    fs: VFS<S, M>,
    mount_point: impl AsRef<Path>,
    concurrency: FuseConcurrencyConfig,
) -> std::io::Result<asyncfuse::raw::MountHandle>
where
    S: BlockStore + Send + Sync + 'static,
    M: MetaLayer + Send + Sync + 'static,
{
    let mount_point = mount_point.as_ref();
    fs.set_fuse_mount_point(mount_point)?;
    let identity = fs.clone();
    if fuse_op_log_enabled() {
        configure_session(
            asyncfuse::raw::Session::new(default_mount_options()),
            concurrency,
        )
        .mount(LoggingFileSystem::new(fs), mount_point)
        .await
        .and_then(|handle| {
            bind_mount_identity(&identity)?;
            Ok(handle)
        })
    } else {
        configure_session(
            asyncfuse::raw::Session::new(default_mount_options()),
            concurrency,
        )
        .mount(fs, mount_point)
        .await
        .and_then(|handle| {
            bind_mount_identity(&identity)?;
            Ok(handle)
        })
    }
}

/// Fallback stub for non-Linux targets (unprivileged).
#[cfg(not(target_os = "linux"))]
pub async fn mount_vfs_unprivileged<S, M>(
    _fs: VFS<S, M>,
    _mount_point: impl AsRef<Path>,
    _concurrency: FuseConcurrencyConfig,
) -> std::io::Result<asyncfuse::raw::MountHandle>
where
    S: BlockStore + Send + Sync + 'static,
    M: MetaLayer + Send + Sync + 'static,
{
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "FUSE mount is only supported on Linux in this build",
    ))
}

/// Fallback stub for non-Linux targets (privileged).
#[cfg(not(target_os = "linux"))]
pub async fn mount_vfs_privileged<S, M>(
    _fs: VFS<S, M>,
    _mount_point: impl AsRef<Path>,
    _concurrency: FuseConcurrencyConfig,
) -> std::io::Result<asyncfuse::raw::MountHandle>
where
    S: BlockStore + Send + Sync + 'static,
    M: MetaLayer + Send + Sync + 'static,
{
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "FUSE mount is only supported on Linux in this build",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_mount_options_request_large_read_requests() {
        let options = default_mount_options();
        let debug = format!("{options:?}");

        assert!(
            debug.contains("max_read=4194304"),
            "Linux mount options should request 4 MiB FUSE read requests: {debug}"
        );
    }

    #[test]
    fn default_mount_options_enable_kernel_permission_checks() {
        let options = default_mount_options();
        let debug = format!("{options:?}");

        assert!(
            debug.contains("default_permissions: true"),
            "BrewFS needs kernel checks for special-node opens such as FIFO permissions: {debug}"
        );
    }

    #[test]
    fn fuse_writeback_cache_defaults_off_and_can_be_enabled() {
        assert!(!parse_fuse_writeback_enabled(None));
        assert!(parse_fuse_writeback_enabled(Some("1".to_string())));
        assert!(parse_fuse_writeback_enabled(Some("true".to_string())));
        assert!(parse_fuse_writeback_enabled(Some("yes".to_string())));
        assert!(parse_fuse_writeback_enabled(Some("on".to_string())));
        assert!(!parse_fuse_writeback_enabled(Some("0".to_string())));
        assert!(!parse_fuse_writeback_enabled(Some("false".to_string())));
        assert!(!parse_fuse_writeback_enabled(Some("maybe".to_string())));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mountinfo_path_decoder_handles_kernel_escapes() {
        assert_eq!(
            decode_mountinfo_path(r"/tmp/brew\040fs\011root"),
            std::path::PathBuf::from("/tmp/brew fs\troot")
        );
    }
}
