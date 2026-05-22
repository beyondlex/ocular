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
    #[serde(default)]
    pub exclude: std::collections::HashMap<String, ExcludeConfig>,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub theme_overrides: Option<ocular_tui::ThemeConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProxyConfig {
    pub name: String,
    pub protocol: String,
    pub listen: String,
    pub remote: String,
    #[serde(default)]
    pub exclude: Option<ExcludeConfig>,
    #[serde(default)]
    pub include: Option<ExcludeConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExcludeConfig {
    pub patterns: Vec<String>,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub regex: bool,
}

fn load_config() -> Result<(Config, PathBuf)> {
    let candidates = [
        // 1. Current directory
        Some(PathBuf::from("ocular.toml")),
        // 2. XDG_CONFIG_HOME/ocular/ocular.toml
        std::env::var("XDG_CONFIG_HOME").ok().map(|d| PathBuf::from(d).join("ocular/ocular.toml")),
        // 3. ~/.config/ocular/ocular.toml
        dirs::config_dir().map(|d| d.join("ocular/ocular.toml")),
    ];
    for candidate in candidates.iter().flatten() {
        if candidate.exists() {
            let config_dir = candidate.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
            let content = std::fs::read_to_string(candidate)
                .with_context(|| format!("failed to read {}", candidate.display()))?;
            let config: Config = toml::from_str(&content).context("failed to parse config")?;
            return Ok((config, config_dir));
        }
    }
    anyhow::bail!("config not found. Create ocular.toml in current directory or ~/.config/ocular/ocular.toml")
}

fn init_tracing(log_dir: &std::path::Path) {
    std::fs::create_dir_all(log_dir).ok();
    let file_appender = rolling::never(log_dir, "ocular.log");
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
    let (config, config_dir) = load_config()?;
    init_tracing(&config_dir);
    info!(proxies = config.proxy.len(), config_dir = %config_dir.display(), "ocular starting");

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
        // Global exclude for this protocol
        let global_exclude = config.exclude.get(&p.protocol).map(|e| ocular_tui::ExcludeConfig {
            patterns: e.patterns.clone(),
            case_sensitive: e.case_sensitive,
            regex: e.regex,
        });
        // Per-proxy exclude (merged with global)
        let local_exclude = p.exclude.as_ref().map(|e| ocular_tui::ExcludeConfig {
            patterns: e.patterns.clone(),
            case_sensitive: e.case_sensitive,
            regex: e.regex,
        });
        let exclude = match (global_exclude, local_exclude) {
            (Some(g), Some(l)) => Some(vec![g, l]),
            (Some(g), None) => Some(vec![g]),
            (None, Some(l)) => Some(vec![l]),
            (None, None) => None,
        };
        let include = p.include.as_ref().map(|e| ocular_tui::ExcludeConfig {
            patterns: e.patterns.clone(),
            case_sensitive: e.case_sensitive,
            regex: e.regex,
        });
        ocular_tui::ComponentInfo {
            name: p.name.clone(),
            listen: p.listen.clone(),
            exclude,
            include,
        }
    }).collect();

    let base_theme = ocular_tui::Theme::by_name(config.theme.as_deref().unwrap_or("default"));
    let theme = if let Some(ref overrides) = config.theme_overrides {
        ocular_tui::Theme::from_config(overrides, &base_theme)
    } else {
        base_theme
    };

    ocular_tui::run(rx, components, theme).await
}
