use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;
use tokio::sync::broadcast;
use tracing::info;
use tracing_appender::rolling;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Debug, Deserialize)]
pub struct Config {
    pub proxy: Vec<ProxyConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProxyConfig {
    pub name: String,
    pub protocol: String,
    pub listen: String,
    pub remote: String,
}

fn load_config() -> Result<Config> {
    let path = PathBuf::from("ocular.toml");
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&content).context("failed to parse config")
}

fn init_tracing() {
    let file_appender = rolling::never(".", "ocular.log");
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(filter)
        .with_writer(file_appender)
        .with_ansi(false)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = load_config()?;
    info!(proxies = config.proxy.len(), "ocular starting");

    let (tx, _) = broadcast::channel::<ocular_proxy::ProxyEvent>(1024);

    for proxy_cfg in &config.proxy {
        let tx = tx.clone();
        let cfg = proxy_cfg.clone();
        let protocol = ocular_protocol::Protocol::from_str(&cfg.protocol)
            .unwrap_or_else(|| {
                tracing::warn!(protocol = %cfg.protocol, "unknown protocol, defaulting to redis");
                ocular_protocol::Protocol::Redis
            });
        tokio::spawn(async move {
            if let Err(e) = ocular_proxy::run_proxy(cfg.listen, cfg.remote, cfg.name, protocol, tx).await {
                tracing::error!(error = %e, "proxy fatal error");
            }
        });
    }

    let rx = tx.subscribe();
    let components: Vec<ocular_tui::ComponentInfo> = config.proxy.iter().map(|p| {
        ocular_tui::ComponentInfo { name: p.name.clone(), listen: p.listen.clone() }
    }).collect();

    ocular_tui::run(rx, components).await
}
