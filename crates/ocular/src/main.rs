use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::info;
use tracing_appender::rolling;
use tracing_subscriber::{fmt, EnvFilter};

mod demo;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub proxy: Vec<ProxyConfig>,
    #[serde(default)]
    pub exclude: std::collections::HashMap<String, ExcludeConfig>,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub theme_overrides: Option<ocular_tui::ThemeConfig>,
    #[serde(default)]
    pub event_format: Option<String>,
    #[serde(default)]
    pub event_log: Option<EventLogConfig>,
    #[serde(default = "default_true")]
    pub leader_menu: bool,
    #[serde(default = "default_true")]
    pub quit_confirm: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventLogConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub include_response: bool,
    #[serde(default)]
    pub components: Vec<String>,
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ProxyConfig {
    pub name: String,
    pub protocol: String,
    #[serde(default)]
    pub listen: String,
    pub remote: String,
    #[serde(default)]
    pub mode: ProxyMode,
    #[serde(default)]
    pub interface: Option<String>,
    #[serde(default)]
    pub exclude: Option<ExcludeConfig>,
    #[serde(default)]
    pub include: Option<ExcludeConfig>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    #[default]
    Proxy,
    Capture,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ExcludeConfig {
    pub patterns: Vec<String>,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub regex: bool,
}

/// A running proxy handle: shutdown sender + task join handle
struct ProxyHandle {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    #[allow(dead_code)]
    handle: JoinHandle<()>,
    config: ProxyConfig,
}

/// Notification sent to TUI when proxies change
pub use ocular_tui::ProxyChange;

fn load_config() -> Result<(Config, PathBuf, PathBuf)> {
    let candidates = [
        Some(PathBuf::from("ocular.toml")),
        std::env::var("XDG_CONFIG_HOME").ok().map(|d| PathBuf::from(d).join("ocular/ocular.toml")),
        dirs::config_dir().map(|d| d.join("ocular/ocular.toml")),
    ];
    for candidate in candidates.iter().flatten() {
        if candidate.exists() {
            let config_dir = candidate.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
            let content = std::fs::read_to_string(candidate)
                .with_context(|| format!("failed to read {}", candidate.display()))?;
            let config: Config = toml::from_str(&content).context("failed to parse config")?;
            return Ok((config, config_dir, candidate.clone()));
        }
    }
    anyhow::bail!("config not found. Create ocular.toml in current directory or ~/.config/ocular/ocular.toml")
}

fn validate_config(config: &Config) -> Result<()> {
    use std::collections::HashSet;
    let mut capture_remotes: HashSet<&str> = HashSet::new();
    let mut proxy_remotes: HashSet<&str> = HashSet::new();

    for p in &config.proxy {
        match p.mode {
            ProxyMode::Capture => {
                if p.interface.is_none() {
                    anyhow::bail!("[{}] mode = \"capture\" requires 'interface' field", p.name);
                }
                capture_remotes.insert(&p.remote);
            }
            ProxyMode::Proxy => {
                if p.listen.is_empty() {
                    anyhow::bail!("[{}] mode = \"proxy\" requires 'listen' field", p.name);
                }
                proxy_remotes.insert(&p.remote);
            }
        }
    }

    for remote in &capture_remotes {
        if proxy_remotes.contains(remote) {
            anyhow::bail!(
                "remote '{}' is configured in both proxy and capture mode — this would produce duplicate events",
                remote
            );
        }
    }
    Ok(())
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

fn make_component_info(p: &ProxyConfig, global_excludes: &HashMap<String, ExcludeConfig>) -> ocular_tui::ComponentInfo {
    let global_exclude = global_excludes.get(&p.protocol).map(|e| ocular_tui::ExcludeConfig {
        patterns: e.patterns.clone(),
        case_sensitive: e.case_sensitive,
        regex: e.regex,
    });
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
}

fn apply_proxy_diff(
    handles: &mut HashMap<String, ProxyHandle>,
    new_proxies: &[ProxyConfig],
    tx: &broadcast::Sender<ocular_proxy::ProxyEvent>,
    change_tx: &broadcast::Sender<ProxyChange>,
    global_excludes: &HashMap<String, ExcludeConfig>,
    new_status: ocular_proxy::StatusMap,
) {
    let new_names: HashMap<String, &ProxyConfig> = new_proxies.iter()
        .map(|p| (p.name.clone(), p)).collect();

    let old_names: Vec<String> = handles.keys().cloned().collect();
    for name in &old_names {
        if !new_names.contains_key(name) {
            if let Some(ph) = handles.remove(name) {
                let _ = ph.shutdown_tx.send(true);
                let _ = change_tx.send(ProxyChange::Removed(name.clone()));
            }
        }
    }
    for (name, new_cfg) in &new_names {
        let needs_restart = match handles.get(name.as_str()) {
            None => true,
            Some(ph) => ph.config.listen != new_cfg.listen || ph.config.remote != new_cfg.remote || ph.config.protocol != new_cfg.protocol,
        };
        if needs_restart {
            if let Some(ph) = handles.remove(name.as_str()) {
                let _ = ph.shutdown_tx.send(true);
                let _ = change_tx.send(ProxyChange::Removed(name.clone()));
            }
            let ph = spawn_proxy(new_cfg, tx, new_status.clone());
            handles.insert(name.clone(), ph);
            let ci = make_component_info(new_cfg, global_excludes);
            let _ = change_tx.send(ProxyChange::Added(ci));
        }
    }
}

fn spawn_proxy(
    cfg: &ProxyConfig,
    tx: &broadcast::Sender<ocular_proxy::ProxyEvent>,
    status: ocular_proxy::StatusMap,
) -> ProxyHandle {
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let tx = tx.clone();
    let cfg_clone = cfg.clone();
    let protocol = ocular_protocol::Protocol::parse(&cfg.protocol)
        .unwrap_or_else(|| {
            tracing::warn!(protocol = %cfg.protocol, "unknown protocol, defaulting to redis");
            ocular_protocol::Protocol::Redis
        });

    let handle = match cfg.mode {
        ProxyMode::Capture => {
            let capture_cfg = ocular_capture::CaptureConfig {
                name: cfg.name.clone(),
                protocol,
                interface: cfg.interface.clone().unwrap_or_else(|| if cfg!(target_os = "macos") { "lo0" } else { "lo" }.to_string()),
                remote: cfg.remote.clone(),
            };
            let name = cfg.name.clone();
            let shutdown_rx = shutdown_rx.clone();
            tokio::spawn(async move {
                if let Err(e) = ocular_capture::run_capture(capture_cfg, tx.clone(), shutdown_rx).await {
                    let _ = tx.send(ocular_proxy::ProxyEvent::system_event(&name, format!("capture error: {}", e)));
                    tracing::error!(error = %e, "capture fatal error");
                }
            })
        }
        ProxyMode::Proxy => {
            let listen = cfg.listen.clone();
            let remote = cfg.remote.clone();
            let name = cfg.name.clone();
            let status = status.clone();
            tokio::spawn(async move {
                if let Err(e) = ocular_proxy::run_proxy(listen, remote, name, protocol, tx, shutdown_rx, status).await {
                    tracing::error!(error = %e, "proxy fatal error");
                }
            })
        }
    };

    ProxyHandle { shutdown_tx, handle, config: cfg_clone }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "-v" || a == "--version") {
        println!("ocular {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if args.iter().any(|a| a == "--demo") {
        let (tx, _) = broadcast::channel::<ocular_proxy::ProxyEvent>(1024);
        let tx2 = tx.clone();
        tokio::spawn(async move { demo::run_demo(tx2).await });
        let rx = tx.subscribe();
        let components = demo::demo_components();
        let theme = ocular_tui::Theme::by_name("tokyo-night-storm");
        let config_path = PathBuf::from("ocular.toml");
        let demo_status: ocular_proxy::StatusMap = Arc::new(Mutex::new(std::collections::HashMap::new()));
        return ocular_tui::run(rx, components, theme, config_path.clone(), None, true, false, None, None, None, None, config_path, demo_status).await;
    }

    let (config, config_dir, config_path) = load_config()?;
    validate_config(&config)?;
    init_tracing(&config_dir);
    info!(proxies = config.proxy.len(), config_dir = %config_dir.display(), "ocular starting");

    // Initialize group directory
    let group_dir = config_dir.join("group");
    std::fs::create_dir_all(&group_dir).ok();

    // Determine active config file: main config proxies → "default" group
    let (active_config_path, active_group) = if config.proxy.is_empty() {
        // No proxies in main config, try first available group
        let mut groups: Vec<String> = std::fs::read_dir(&group_dir).ok()
            .map(|entries| entries.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    if p.extension().is_some_and(|ext| ext == "toml") {
                        p.file_stem().and_then(|s| s.to_str().map(String::from))
                    } else { None }
                }).collect())
            .unwrap_or_default();
        groups.sort();
        if let Some(first) = groups.first() {
            (group_dir.join(format!("{}.toml", first)), Some(first.clone()))
        } else {
            (config_path.clone(), Some("default".to_string()))
        }
    } else {
        // Main config has proxies → treat as "default" group
        (config_path.clone(), Some("default".to_string()))
    };

    // Load proxies from active config
    let active_proxies: Vec<ProxyConfig> = if active_config_path != config_path {
        std::fs::read_to_string(&active_config_path).ok()
            .and_then(|c| toml::from_str::<Config>(&c).ok())
            .map(|c| c.proxy)
            .unwrap_or_default()
    } else {
        config.proxy.clone()
    };

    let (tx, _) = broadcast::channel::<ocular_proxy::ProxyEvent>(1024);
    let (proxy_change_tx, proxy_change_rx) = broadcast::channel::<ProxyChange>(64);
    let status_map: ocular_proxy::StatusMap = Arc::new(Mutex::new(std::collections::HashMap::new()));

    // Don't spawn proxies at startup — wait for user to select a group in Dashboard
    let proxy_handles: HashMap<String, ProxyHandle> = HashMap::new();

    // Proxy hot-reload watcher task
    let _config_path_watch = active_config_path.clone();
    let tx_reload = tx.clone();
    let proxy_change_tx_clone = proxy_change_tx.clone();
    let initial_excludes = config.exclude.clone();
    let _group_dir_watch = group_dir.clone();
    let status_watcher = status_map.clone();
    tokio::spawn(async move {
        let mut last_mtime = SystemTime::UNIX_EPOCH;
        let mut handles = proxy_handles;
        #[allow(unused_assignments)]
        let mut global_excludes: std::collections::HashMap<String, ExcludeConfig> = initial_excludes;
        let mut watch_path: Option<PathBuf> = None; // No watching until a group is loaded
        let mut change_rx = proxy_change_tx_clone.subscribe();
        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                    let Some(ref wp) = watch_path else { continue };
                    let mtime = match std::fs::metadata(wp).and_then(|m| m.modified()) {
                        Ok(t) => t,
                        Err(_) => continue,
                    };
                    if mtime == last_mtime { continue; }
                    last_mtime = mtime;

                    let content = match std::fs::read_to_string(wp) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    let new_config: Config = match toml::from_str(&content) {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(error = %e, "config parse error during hot-reload");
                            continue;
                        }
                    };

                    global_excludes = new_config.exclude.clone();
                    apply_proxy_diff(&mut handles, &new_config.proxy, &tx_reload, &proxy_change_tx_clone, &global_excludes, status_watcher.clone());
                }
                Ok(change) = change_rx.recv() => {
                    match change {
                        ProxyChange::SwitchGroup(new_path) => {
                            for (name, ph) in handles.drain() {
                                let _ = ph.shutdown_tx.send(true);
                                info!(component = %name, "proxy stopped for group switch");
                            }
                            watch_path = Some(new_path.clone());
                            last_mtime = SystemTime::UNIX_EPOCH;
                            if let Ok(content) = std::fs::read_to_string(&new_path) {
                                if let Ok(new_config) = toml::from_str::<Config>(&content) {
                                    for proxy_cfg in &new_config.proxy {
                                        let ph = spawn_proxy(proxy_cfg, &tx_reload, status_watcher.clone());
                                        handles.insert(proxy_cfg.name.clone(), ph);
                                        info!(component = %proxy_cfg.name, "proxy started for group switch");
                                    }
                                }
                            }
                        }
                        ProxyChange::StopAll => {
                            for (name, ph) in handles.drain() {
                                let _ = ph.shutdown_tx.send(true);
                                info!(component = %name, "proxy stopped (StopAll)");
                            }
                            watch_path = None;
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    // Event logger
    let event_log_enabled = config.event_log.as_ref().is_some_and(|c| c.enabled);
    let include_response = config.event_log.as_ref().is_some_and(|c| c.include_response);
    let log_components: Vec<String> = config.event_log.as_ref().map_or(vec![], |c| c.components.clone());
    if event_log_enabled {
        let event_log_path = config_dir.join("events.log");
        let mut event_rx = tx.subscribe();
        tokio::spawn(async move {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true).append(true).open(&event_log_path)
                .expect("failed to open events.log");
            while let Ok(ev) = event_rx.recv().await {
                if !log_components.is_empty() && !log_components.contains(&ev.component) {
                    continue;
                }
                let ts: DateTime<Local> = ev.timestamp.into();
                let command = ev.full_command.replace('\n', " ");
                let addr = match (&ev.src, &ev.dest) {
                    (Some(s), Some(d)) => format!(" {} → {}", s, d),
                    _ => String::new(),
                };
                if include_response {
                    let response = ev.response.replace('\n', " ");
                    let _ = writeln!(file, "{} [{}]{} {} ({:.2}ms) -> {}",
                        ts.format("%H:%M:%S%.3f"),
                        ev.component,
                        addr,
                        command,
                        ev.latency.as_secs_f64() * 1000.0,
                        response,
                    );
                } else {
                    let _ = writeln!(file, "{} [{}]{} {} ({:.2}ms)",
                        ts.format("%H:%M:%S%.3f"),
                        ev.component,
                        addr,
                        command,
                        ev.latency.as_secs_f64() * 1000.0,
                    );
                }
            }
        });
    }

    let rx = tx.subscribe();
    let components: Vec<ocular_tui::ComponentInfo> = active_proxies.iter()
        .map(|p| make_component_info(p, &config.exclude))
        .collect();

    let base_theme = ocular_tui::Theme::by_name(config.theme.as_deref().unwrap_or("default"));
    let theme = if let Some(ref overrides) = config.theme_overrides {
        ocular_tui::Theme::from_config(overrides, &base_theme)
    } else {
        base_theme
    };

    ocular_tui::run(rx, components, theme, active_config_path, config.event_format, config.leader_menu, config.quit_confirm, Some(proxy_change_rx), Some(group_dir), active_group, Some(proxy_change_tx), config_path, status_map).await
}
