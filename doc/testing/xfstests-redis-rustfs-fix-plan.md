# xfstests Redis + RustFS Fix Map

Short handoff for the Redis metadata + RustFS object-store correctness run.

## Fixed

- Empty FUSE env values no longer enable `direct_io`; default opens are buffered
  and mmap-safe. Unit: `open_reply_flags`.
- FUSE `copy_file_range` now defaults to `EOPNOTSUPP`, avoiding the kernel
  `copy_file_range` hang. Opt in with `BREWFS_FUSE_COPY_FILE_RANGE=1`.
- FUSE OFD/POSIX locks now track `fh -> lock_owner` and release on `release(fh)`.
  This fixes the `generic/478` hang, but not the full OFD close/refcount
  semantics.
- Redis xfstests harness disables Redis RDB/AOF persistence, constrains BrewFS
  cache budgets, and bind-mounts RustFS data outside Docker named volumes.

## Case Map

| Case | Status | Artifact | Notes / next touch |
| --- | --- | --- | --- |
| `generic/075` | PASS | `run-1783115670-19341` | fsx non-AIO smoke passed in the passing focused set. |
| `generic/112` | EXCLUDE | `run-1783114304-1421` | AIO fsx now fails with mmap stale data in `112.0.full` after `MAPWRITE` then `MAPREAD`. Same family as `263`; next touch is FUSE page-cache invalidation/mmap strategy. |
| `generic/080 generic/438` | PASS | `run-1783115670-19341` | Keep `BREWFS_FUSE_WRITEBACK` off for correctness. |
| `generic/198 215 246 247 248 428 437` | PASS | `run-1783115670-19341` | Passed in the passing focused set. |
| `generic/478` | EXCLUDE | `run-1783114304-1421` | No longer hangs, but OFD lock output mismatches remain. Needs async-fuse/FUSE lock owner/refcount work. |
| `generic/074` | EXCLUDE | `run-1783106980-9036` | `fstest -b512` projected ~4.6M tiny RustFS objects/inodes for one case. Harness no longer has Redis/S3 errors; backend cost is impractical. |
| `generic/263` | EXCLUDE | `run-1783113101-26238` | Stable mmap+truncate stale page at op 2015/2016/2018: `MAPWRITE 0x39000..0x5618a`, truncate to `0x3c000`, then stale read at `0x39523`. Userspace ordering/lock fix did not clear it. Next touch: async-fuse `Notify::invalid_inode` plumbing or a stricter mmap strategy. |

## Commands

```bash
export DOCKER_HOST=unix:///tmp/brewfs-docker4.sock
unset BREWFS_FUSE_DIRECT_IO BREWFS_FUSE_READ_DIRECT_IO BREWFS_FUSE_WRITE_DIRECT_IO
unset BREWFS_FUSE_WRITEBACK BREWFS_FUSE_COPY_FILE_RANGE RUSTFS_DATA_HOST_DIR
export BREWFS_UPLOAD_CONCURRENCY=2

bash docker/compose-xfstests/run_redis_xfstests.sh --cases "generic/075 generic/080 generic/198 generic/215 generic/246 generic/247 generic/248 generic/428 generic/437 generic/438"
bash docker/compose-xfstests/run_redis_perf.sh --writeback-throughput-profile --tools "fio-seqwrite fio-bigwrite fio-randrw metaperf"
```

## Notes

- `BREWFS_FUSE_WRITEBACK=1` remains unsafe for correctness runs.
- `generic/112` and `generic/263` are not RustFS/Redis failures; they are mmap
  stale-data cases through the kernel/FUSE page cache.
