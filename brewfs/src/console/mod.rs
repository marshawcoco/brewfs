pub mod api;
pub mod server;

use crate::config::ConsoleArgs;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMode {
    Disabled,
    Token,
}

#[derive(Debug, Clone)]
pub struct ConsoleConfig {
    pub listen: SocketAddr,
    pub static_dir: PathBuf,
    pub auth_mode: AuthMode,
    pub csi_dashboard: bool,
}

#[derive(Debug, Clone)]
pub struct ConsoleState {
    pub auth_mode: AuthMode,
    pub static_dir: PathBuf,
    pub csi_dashboard: bool,
}

impl ConsoleConfig {
    pub fn from_args(args: ConsoleArgs) -> anyhow::Result<Self> {
        let auth_mode = if args.dev_no_auth {
            ensure_loopback(args.listen)?;
            AuthMode::Disabled
        } else {
            AuthMode::Token
        };

        Ok(Self {
            listen: args.listen,
            static_dir: args
                .static_dir
                .unwrap_or_else(|| PathBuf::from("brewfs/web/console/dist")),
            auth_mode,
            csi_dashboard: args.enable_csi_dashboard,
        })
    }
}

pub async fn serve_cmd(args: ConsoleArgs) -> anyhow::Result<()> {
    let config = ConsoleConfig::from_args(args)?;
    server::serve(config).await
}

fn ensure_loopback(listen: SocketAddr) -> anyhow::Result<()> {
    let is_loopback = match listen.ip() {
        IpAddr::V4(ip) => ip.is_loopback(),
        IpAddr::V6(ip) => ip.is_loopback(),
    };
    if is_loopback {
        Ok(())
    } else {
        anyhow::bail!("--dev-no-auth is only allowed with loopback listen addresses")
    }
}
