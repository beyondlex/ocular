use anyhow::Result;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ocular_proxy::{ProxyEvent, StatusMap};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
};
use serde::Deserialize;
use std::io::stdout;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast;

mod theme;
pub use theme::{Theme, ThemeConfig};

#[derive(Debug, Clone)]
pub struct ComponentInfo {
    pub name: String,
    pub listen: String,
    pub exclude: Option<Vec<ExcludeConfig>>,
    pub include: Option<ExcludeConfig>,
}

#[derive(Debug, Clone)]
pub struct ExcludeConfig {
    pub patterns: Vec<String>,
    pub case_sensitive: bool,
    pub regex: bool,
}

/// Compiled exclude matcher for a component
struct ExcludeMatcher {
    excludes: Vec<MatcherKind>,
    includes: Vec<MatcherKind>,
}

enum MatcherKind {
    Regex(regex::Regex),
    Plain { pattern: String, case_sensitive: bool },
}

impl ExcludeMatcher {
    fn compile_patterns(cfg: &ExcludeConfig) -> Vec<MatcherKind> {
        cfg.patterns.iter().filter_map(|p| {
            if cfg.regex {
                let pat = if cfg.case_sensitive { p.clone() } else { format!("(?i){}", p) };
                regex::Regex::new(&pat).ok().map(MatcherKind::Regex)
            } else {
                let pattern = if cfg.case_sensitive { p.clone() } else { p.to_lowercase() };
                Some(MatcherKind::Plain { pattern, case_sensitive: cfg.case_sensitive })
            }
        }).collect()
    }

    fn new(excludes: Option<&Vec<ExcludeConfig>>, include: Option<&ExcludeConfig>) -> Self {
        let exclude_matchers = excludes.map(|cfgs| {
            cfgs.iter().flat_map(Self::compile_patterns).collect()
        }).unwrap_or_default();
        Self {
            excludes: exclude_matchers,
            includes: include.map(Self::compile_patterns).unwrap_or_default(),
        }
    }

    fn matches_any(matchers: &[MatcherKind], text: &str) -> bool {
        matchers.iter().any(|m| match m {
            MatcherKind::Regex(re) => re.is_match(text),
            MatcherKind::Plain { pattern, case_sensitive } => {
                if *case_sensitive { text.contains(pattern.as_str()) }
                else { text.to_lowercase().contains(pattern.as_str()) }
            }
        })
    }

    fn is_excluded(&self, text: &str) -> bool {
        if self.excludes.is_empty() { return false; }
        // include overrides exclude
        if !self.includes.is_empty() && Self::matches_any(&self.includes, text) {
            return false;
        }
        Self::matches_any(&self.excludes, text)
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Focus { Components, Events, Detail, Filter, ComponentFilter }

#[derive(PartialEq, Clone)]
enum AppMode {
    Dashboard,
    GroupDetail,
    NewGroupName,
    NewGroupAddProxy,
    RenameGroup,
    Main,
}

struct DashboardState {
    groups: Vec<DashboardGroup>,
    selected: usize,
    filter: String,
    filter_active: bool,
    new_group_name: String,
    new_group_proxies: Vec<NewProxyEntry>,
    error: Option<String>,
    rename_input: String,
    delete_confirm: bool,
    detail_proxies: Vec<NewProxyEntry>,
    detail_selected: usize,
    detail_group_name: String,
    detail_delete_confirm: bool,
    fuzzy_matcher: SkimMatcherV2,
}

#[derive(Clone)]
struct DashboardGroup {
    name: String,
    proxies: Vec<String>, // proxy names for display
}

#[derive(Clone)]
struct NewProxyEntry {
    name: String,
    protocol: String,
    listen: String,
    remote: String,
    mode: String,
    interface: String,
}

impl DashboardState {
    fn load(group_dir: &std::path::Path, main_config: &std::path::Path) -> Self {
        let mut groups = Vec::new();
        // "default" from main config
        if let Ok(content) = std::fs::read_to_string(main_config) {
            if let Ok(cfg) = toml::from_str::<ReloadableConfig>(&content) {
                if !cfg.proxy.is_empty() {
                    groups.push(DashboardGroup {
                        name: "default".to_string(),
                        proxies: cfg.proxy.iter().map(|p| p.name.clone()).collect(),
                    });
                }
            }
        }
        // Groups from group dir
        if let Ok(entries) = std::fs::read_dir(group_dir) {
            let mut group_files: Vec<_> = entries.flatten()
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
                .collect();
            group_files.sort_by_key(|e| e.file_name());
            for entry in group_files {
                let path = entry.path();
                let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let proxies = std::fs::read_to_string(&path).ok()
                    .and_then(|c| toml::from_str::<ReloadableConfig>(&c).ok())
                    .map(|cfg| cfg.proxy.iter().map(|p| p.name.clone()).collect())
                    .unwrap_or_default();
                groups.push(DashboardGroup { name, proxies });
            }
        }
        Self { groups, selected: 0, filter: String::new(), filter_active: false, new_group_name: String::new(), new_group_proxies: Vec::new(), error: None, rename_input: String::new(), delete_confirm: false, detail_proxies: Vec::new(), detail_selected: 0, detail_group_name: String::new(), detail_delete_confirm: false, fuzzy_matcher: SkimMatcherV2::default() }
    }

    fn filtered_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            (0..self.groups.len()).collect()
        } else {
            self.groups.iter().enumerate().filter(|(_, g)| {
                self.fuzzy_matcher.fuzzy_match(&g.name, &self.filter).is_some()
            }).map(|(i, _)| i).collect()
        }
    }
}

const PROTOCOLS: &[&str] = &["redis", "mysql", "postgres", "amqp", "mongodb", "http", "memcached", "kafka"];

fn default_port(protocol: &str) -> &'static str {
    match protocol {
        "redis" => "6379",
        "mysql" => "3306",
        "postgres" => "5432",
        "amqp" => "5672",
        "mongodb" => "27017",
        "http" => "9200",
        "memcached" => "11211",
        "kafka" => "9092",
        _ => "",
    }
}

/// Fields: 0=name, 1=protocol(selector), 2=remote_host, 3=remote_port
#[derive(Default)]
struct ProxyForm {
    /// [name, listen_host, listen_port, remote_host, remote_port, interface]
    fields: [String; 6],
    active_field: usize,
    editing_idx: Option<usize>,
    protocol_idx: usize,
    mode_idx: usize, // 0=proxy, 1=capture
    error: Option<String>,
    /// Existing listen addr for edit mode (reused on save)
    existing_listen: Option<String>,
}

const MODES: &[&str] = &["proxy", "capture"];

impl ProxyForm {
    /// Map active_field (row index) to fields[] index.
    /// Proxy create: rows [name, protocol, mode, remote_host, remote_port] → fields [0, -, -, 3, 4]
    /// Capture create: rows [name, protocol, mode, remote_host, remote_port, interface] → fields [0, -, -, 3, 4, 5]
    /// Proxy edit: rows [name, protocol, mode, listen_host, listen_port, remote_host, remote_port] → fields [0, -, -, 1, 2, 3, 4]
    /// Capture edit: rows [name, protocol, mode, remote_host, remote_port, interface] → fields [0, -, -, 3, 4, 5]
    fn field_idx(&self) -> Option<usize> {
        let is_capture = self.mode_idx == 1;
        if self.editing_idx.is_some() && !is_capture {
            // Proxy edit: name, protocol, mode, listen_host, listen_port, remote_host, remote_port
            match self.active_field {
                0 => Some(0),
                1 | 2 => None, // protocol, mode (selectors)
                3 => Some(1),
                4 => Some(2),
                5 => Some(3),
                6 => Some(4),
                _ => None,
            }
        } else if is_capture {
            // Capture (create or edit): name, protocol, mode, remote_host, remote_port, interface
            match self.active_field {
                0 => Some(0),
                1 | 2 => None, // protocol, mode (selectors)
                3 => Some(3),
                4 => Some(4),
                5 => Some(5),
                _ => None,
            }
        } else {
            // Proxy create: name, protocol, mode, remote_host, remote_port
            match self.active_field {
                0 => Some(0),
                1 | 2 => None, // protocol, mode
                3 => Some(3),
                4 => Some(4),
                _ => None,
            }
        }
    }

    fn row_count(&self) -> usize {
        let is_capture = self.mode_idx == 1;
        if is_capture {
            6 // name, protocol, mode, remote_host, remote_port, interface
        } else if self.editing_idx.is_some() {
            7 // name, protocol, mode, listen_host, listen_port, remote_host, remote_port
        } else {
            5 // name, protocol, mode, remote_host, remote_port
        }
    }

    fn from_entry(entry: &NewProxyEntry) -> Self {
        let protocol_idx = PROTOCOLS.iter().position(|&p| p == entry.protocol).unwrap_or(0);
        let mode_idx = if entry.mode == "capture" { 1 } else { 0 };
        let (rh, rp) = split_addr(&entry.remote);
        let (lh, lp) = split_addr(&entry.listen);
        Self {
            fields: [entry.name.clone(), lh, lp, rh, rp, entry.interface.clone()],
            active_field: 0,
            editing_idx: None,
            protocol_idx,
            mode_idx,
            error: None,
            existing_listen: Some(entry.listen.clone()),
        }
    }
}

fn auto_assign_listen_port(protocol: &str) -> String {
    use std::net::TcpListener;
    let base: u16 = match default_port(protocol).parse::<u16>() {
        Ok(p) => p.saturating_add(10000),
        Err(_) => 20000,
    };
    for offset in 0..1000 {
        let port = base + offset;
        let addr = format!("127.0.0.1:{}", port);
        if TcpListener::bind(&addr).is_ok() {
            return addr;
        }
    }
    format!("127.0.0.1:{}", base)
}

fn split_addr(addr: &str) -> (String, String) {
    if let Some((h, p)) = addr.rsplit_once(':') {
        (h.to_string(), p.to_string())
    } else {
        (addr.to_string(), String::new())
    }
}

struct GroupPicker {
    groups: Vec<String>,
    selected: usize,
}

/// Per-component aggregate statistics
#[derive(Default)]
struct ComponentStats {
    count: u64,
    error_count: u64,
    latency_sum: Duration,
    latency_min: Duration,
    latency_max: Duration,
    /// Sorted latencies for p95 (capped to avoid unbounded memory)
    latencies: Vec<Duration>,
    first_event: Option<SystemTime>,
    last_event: Option<SystemTime>,
}

impl ComponentStats {
    fn record(&mut self, ev: &ProxyEvent) {
        self.count += 1;
        if ev.response.starts_with("ERR") || ev.response.starts_with("ERROR") {
            self.error_count += 1;
        }
        if ev.system { return; }
        let lat = ev.latency;
        if lat > Duration::ZERO {
            self.latency_sum += lat;
            if self.latency_min == Duration::ZERO || lat < self.latency_min {
                self.latency_min = lat;
            }
            if lat > self.latency_max {
                self.latency_max = lat;
            }
            self.latencies.push(lat);
        }
        let ts = ev.timestamp;
        if self.first_event.is_none() { self.first_event = Some(ts); }
        self.last_event = Some(ts);
    }

    fn avg_latency(&self) -> Duration {
        let n = self.latencies.len() as u64;
        if n == 0 { return Duration::ZERO; }
        self.latency_sum / n as u32
    }

    fn p95_latency(&self) -> Duration {
        if self.latencies.is_empty() { return Duration::ZERO; }
        let mut sorted = self.latencies.clone();
        sorted.sort();
        let idx = (sorted.len() as f64 * 0.95) as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    fn qps(&self) -> f64 {
        match (self.first_event, self.last_event) {
            (Some(first), Some(last)) => {
                let elapsed = last.duration_since(first).unwrap_or(Duration::ZERO).as_secs_f64();
                if elapsed < 1.0 { self.count as f64 } else { self.count as f64 / elapsed }
            }
            _ => 0.0,
        }
    }

    fn error_rate(&self) -> f64 {
        if self.count == 0 { return 0.0; }
        self.error_count as f64 / self.count as f64 * 100.0
    }
}

struct App {
    events: Vec<ProxyEvent>,
    selected: usize,
    detail_scroll: u16,
    focus: Focus,
    components: Vec<ComponentInfo>,
    component_idx: Option<usize>,
    filter: String,
    pending_keys: String,
    leader_active: bool,
    show_leader_menu: bool,
    help_active: bool,
    confirm_quit: bool,
    quit_confirm_enabled: bool,
    visual_mode: bool,
    visual_anchor: usize,
    theme: Theme,
    paused: bool,
    paused_buffer: Vec<ocular_protocol::ProxyEvent>,
    follow: bool,
    exclude_matchers: std::collections::HashMap<String, ExcludeMatcher>,
    event_format: EventFormat,
    latency_threshold_ms: Option<f64>,
    fuzzy_filter: bool,
    proxy_form: Option<ProxyForm>,
    delete_confirm_idx: Option<usize>,
    info_popup_idx: Option<usize>,
    component_filter: String,
    config_path: PathBuf,
    group_dir: Option<PathBuf>,
    active_group: Option<String>,
    group_picker: Option<GroupPicker>,
    proxy_change_tx: Option<broadcast::Sender<ProxyChange>>,
    mode: AppMode,
    dashboard: DashboardState,
    main_config_path: PathBuf,
    status_map: StatusMap,
    component_stats: std::collections::HashMap<String, ComponentStats>,
    // Cached resources
    fuzzy_matcher: SkimMatcherV2,
    dirty: bool,
    cached_filtered_indices: Vec<usize>,
    /// Cache key: (events.len, filter, component_idx, fuzzy_filter)
    /// None = cache needs recompute
    cached_filter_key: Option<(usize, String, Option<usize>, bool)>,
}

fn filtered_component_indices(app: &App) -> Vec<usize> {
    if app.component_filter.is_empty() {
        (0..app.components.len()).collect()
    } else {
        app.components.iter().enumerate().filter(|(_, c)| {
            let target = format!("{} {} {}", c.name, c.listen, c.listen);
            app.fuzzy_matcher.fuzzy_match(&target, &app.component_filter).is_some()
        }).map(|(i, _)| i).collect()
    }
}

impl App {
    fn filtered_events(&self) -> Vec<(usize, &ProxyEvent, Vec<usize>)> {
        self.events.iter().enumerate().filter_map(|(i, ev)| {
            if let Some(idx) = self.component_idx {
                if let Some(c) = self.components.get(idx) {
                    if ev.component != c.name { return None; }
                }
            }
            if !self.filter.is_empty() {
                if self.fuzzy_filter {
                    if let Some((_, indices)) = self.fuzzy_matcher.fuzzy_indices(&ev.command, &self.filter) {
                        return Some((i, ev, indices));
                    }
                    if self.fuzzy_matcher.fuzzy_match(&ev.component, &self.filter).is_some() {
                        return Some((i, ev, vec![]));
                    }
                    return None;
                } else {
                    let q = self.filter.to_lowercase();
                    if !ev.component.to_lowercase().contains(&q)
                        && !ev.command.to_lowercase().contains(&q) {
                        return None;
                    }
                }
            }
            Some((i, ev, vec![]))
        }).collect()
    }

    fn refresh_filter_cache(&mut self) {
        let key = (self.events.len(), self.filter.clone(), self.component_idx, self.fuzzy_filter);
        if self.cached_filter_key.as_ref() != Some(&key) {
            self.cached_filtered_indices = self.events.iter().enumerate().filter_map(|(i, ev)| {
                if let Some(idx) = self.component_idx {
                    if let Some(c) = self.components.get(idx) {
                        if ev.component != c.name { return None; }
                    }
                }
                if !self.filter.is_empty() {
                    if self.fuzzy_filter {
                        if self.fuzzy_matcher.fuzzy_match(&ev.command, &self.filter).is_some() {
                            return Some(i);
                        }
                        if self.fuzzy_matcher.fuzzy_match(&ev.component, &self.filter).is_some() {
                            return Some(i);
                        }
                        return None;
                    } else {
                        let q = self.filter.to_lowercase();
                        if !ev.component.to_lowercase().contains(&q)
                            && !ev.command.to_lowercase().contains(&q) {
                            return None;
                        }
                    }
                }
                Some(i)
            }).collect();
            self.cached_filter_key = Some(key);
        }
    }
}

/// Event line format template.
/// Syntax: `%field` or `%{width}field` for fixed width.
/// Positive width = right-aligned, negative = left-aligned.
/// Fields: index, time, component, command, latency, process
#[derive(Debug, Clone)]
struct EventFormat {
    segments: Vec<FormatSegment>,
}

#[derive(Debug, Clone)]
enum FormatSegment {
    Literal(String),
    Field { name: String, width: Option<i32> },
}

impl EventFormat {
    fn parse(template: &str) -> Self {
        let mut segments = Vec::new();
        let mut chars = template.chars().peekable();
        let mut literal = String::new();

        while let Some(c) = chars.next() {
            if c == '%' {
                if !literal.is_empty() {
                    segments.push(FormatSegment::Literal(std::mem::take(&mut literal)));
                }
                let width = if chars.peek() == Some(&'{') {
                    chars.next(); // consume '{'
                    let mut w = String::new();
                    while let Some(&ch) = chars.peek() {
                        if ch == '}' { chars.next(); break; }
                        w.push(ch);
                        chars.next();
                    }
                    w.parse::<i32>().ok()
                } else {
                    None
                };
                let mut name = String::new();
                while let Some(&ch) = chars.peek() {
                    if ch.is_alphanumeric() || ch == '_' {
                        name.push(ch);
                        chars.next();
                    } else {
                        break;
                    }
                }
                segments.push(FormatSegment::Field { name, width });
            } else {
                literal.push(c);
            }
        }
        if !literal.is_empty() {
            segments.push(FormatSegment::Literal(literal));
        }
        Self { segments }
    }

    fn default_format() -> Self {
        Self::parse("%{5}index %time [%{-12}component] %command (%latency)")
    }
}

/// Config structure for hot-reload (only the parts we can reload)
#[derive(Debug, Deserialize)]
struct ReloadableConfig {
    #[serde(default)]
    proxy: Vec<ReloadableProxy>,
    #[serde(default)]
    exclude: std::collections::HashMap<String, ReloadableExclude>,
    #[serde(default)]
    theme: Option<String>,
    #[serde(default)]
    theme_overrides: Option<ThemeConfig>,
    #[serde(default)]
    event_format: Option<String>,
    #[serde(default)]
    latency_threshold_ms: Option<f64>,
    #[serde(default = "default_true")]
    fuzzy_filter: bool,
}

#[derive(Debug, Deserialize)]
struct ReloadableProxy {
    name: String,
    #[serde(default)]
    protocol: String,
    #[serde(default)]
    #[allow(dead_code)]
    listen: String,
    #[serde(default)]
    remote: String,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    interface: Option<String>,
    #[serde(default)]
    exclude: Option<ReloadableExclude>,
    #[serde(default)]
    include: Option<ReloadableExclude>,
}

#[derive(Debug, Deserialize)]
struct ReloadableExclude {
    patterns: Vec<String>,
    #[serde(default)]
    case_sensitive: bool,
    #[serde(default)]
    regex: bool,
}

/// Notification sent to TUI when proxies change via hot-reload
#[derive(Debug, Clone)]
pub enum ProxyChange {
    Added(ComponentInfo),
    Removed(String),
    SwitchGroup(PathBuf),
    StopAll,
}

#[allow(clippy::too_many_arguments)]
pub async fn run(
    mut rx: broadcast::Receiver<ProxyEvent>,
    components: Vec<ComponentInfo>,
    theme: Theme,
    config_path: PathBuf,
    event_format: Option<String>,
    show_leader_menu: bool,
    quit_confirm: bool,
    proxy_change_rx: Option<broadcast::Receiver<ProxyChange>>,
    group_dir: Option<PathBuf>,
    active_group: Option<String>,
    proxy_change_tx: Option<broadcast::Sender<ProxyChange>>,
    main_config_path: PathBuf,
    status_map: StatusMap,
) -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let exclude_matchers: std::collections::HashMap<String, ExcludeMatcher> = components.iter()
        .filter(|c| c.exclude.is_some() || c.include.is_some())
        .map(|c| (c.name.clone(), ExcludeMatcher::new(c.exclude.as_ref(), c.include.as_ref())))
        .collect();

    let fmt = event_format.as_deref().map(EventFormat::parse).unwrap_or_else(EventFormat::default_format);
    let app_group_dir = group_dir.clone();

    let mut app = App {
        events: Vec::new(),
        selected: 0,
        detail_scroll: 0,
        focus: Focus::Events,
        components: Vec::new(), // empty — populated when user selects a group
        component_idx: None,
        filter: String::new(),
        pending_keys: String::new(),
        leader_active: false,
        show_leader_menu,
        help_active: false,
        confirm_quit: false,
        quit_confirm_enabled: quit_confirm,
        visual_mode: false,
        visual_anchor: 0,
        theme,
        paused: false,
        paused_buffer: Vec::new(),
        follow: true,
        exclude_matchers,
        event_format: fmt,
        latency_threshold_ms: None,
        fuzzy_filter: true,
        proxy_form: None,
        delete_confirm_idx: None,
        info_popup_idx: None,
        component_filter: String::new(),
        config_path: config_path.clone(),
        group_dir,
        active_group,
        group_picker: None,
        proxy_change_tx,
        mode: AppMode::Dashboard,
        dashboard: DashboardState::load(
            app_group_dir.as_deref().unwrap_or(std::path::Path::new("")),
            &main_config_path,
        ),
        main_config_path: main_config_path.clone(),
        status_map,
        component_stats: std::collections::HashMap::new(),
        fuzzy_matcher: SkimMatcherV2::default(),
        dirty: true,
        cached_filtered_indices: Vec::new(),
        cached_filter_key: None,
    };

    let mut last_mtime = SystemTime::UNIX_EPOCH;
    let mut proxy_change_rx = proxy_change_rx;

    loop {
        // Dashboard / NewGroup modes
        if app.mode != AppMode::Main {
            terminal.draw(|f| ui_dashboard(f, &app))?;

            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press { continue; }
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
                        break;
                    }
                    match &app.mode {
                        AppMode::Dashboard => {
                            if app.dashboard.delete_confirm {
                                match key.code {
                                    KeyCode::Char('y') | KeyCode::Enter => {
                                        if let Some(g) = app.dashboard.groups.get(app.dashboard.selected) {
                                            if let Some(ref gdir) = app.group_dir.clone() {
                                                let file = gdir.join(format!("{}.toml", g.name));
                                                let _ = std::fs::remove_file(&file);
                                                app.dashboard = DashboardState::load(gdir, &app.main_config_path);
                                                if app.dashboard.selected >= app.dashboard.groups.len() {
                                                    app.dashboard.selected = app.dashboard.groups.len().saturating_sub(1);
                                                }
                                            }
                                        }
                                    }
                                    _ => { app.dashboard.delete_confirm = false; }
                                }
                                app.dashboard.delete_confirm = false;
                                continue;
                            }
                            if app.dashboard.filter_active {
                                match key.code {
                                    KeyCode::Esc => { app.dashboard.filter.clear(); app.dashboard.filter_active = false; }
                                    KeyCode::Enter => { app.dashboard.filter_active = false; }
                                    KeyCode::Backspace => { app.dashboard.filter.pop(); }
                                    KeyCode::Char(c) => { app.dashboard.filter.push(c); }
                                    _ => {}
                                }
                                continue;
                            }
                            match key.code {
                                KeyCode::Char('q') => break,
                                KeyCode::Char('j') | KeyCode::Down => {
                                    let visible = app.dashboard.filtered_indices();
                                    if let Some(pos) = visible.iter().position(|&i| i == app.dashboard.selected) {
                                        if pos + 1 < visible.len() { app.dashboard.selected = visible[pos + 1]; }
                                    } else if !visible.is_empty() {
                                        app.dashboard.selected = visible[0];
                                    }
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    let visible = app.dashboard.filtered_indices();
                                    if let Some(pos) = visible.iter().position(|&i| i == app.dashboard.selected) {
                                        if pos > 0 { app.dashboard.selected = visible[pos - 1]; }
                                    } else if !visible.is_empty() {
                                        app.dashboard.selected = *visible.last().unwrap();
                                    }
                                }
                                KeyCode::Char('/') => { app.dashboard.filter_active = true; }
                                KeyCode::Char('n') => {
                                    app.dashboard.new_group_name.clear();
                                    app.dashboard.new_group_proxies.clear();
                                    app.dashboard.error = None;
                                    app.mode = AppMode::NewGroupName;
                                }
                                KeyCode::Char('e') => {
                                    if let Some(g) = app.dashboard.groups.get(app.dashboard.selected) {
                                        if let Some(ref gdir) = app.group_dir {
                                            let file = if g.name == "default" {
                                                app.main_config_path.clone()
                                            } else {
                                                gdir.join(format!("{}.toml", g.name))
                                            };
                                            disable_raw_mode()?;
                                            stdout().execute(LeaveAlternateScreen)?;
                                            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
                                            let _ = std::process::Command::new(&editor).arg(&file).status();
                                            stdout().execute(EnterAlternateScreen)?;
                                            enable_raw_mode()?;
                                            terminal.clear()?;
                                            // Reload dashboard
                                            app.dashboard = DashboardState::load(gdir, &app.main_config_path);
                                        }
                                    }
                                }
                                KeyCode::Char('d') => {
                                    if let Some(g) = app.dashboard.groups.get(app.dashboard.selected) {
                                        if g.name != "default" {
                                            app.dashboard.delete_confirm = true;
                                        }
                                    }
                                }
                                KeyCode::Char('r') => {
                                    if let Some(g) = app.dashboard.groups.get(app.dashboard.selected) {
                                        if g.name != "default" {
                                            app.dashboard.rename_input = g.name.clone();
                                            app.dashboard.error = None;
                                            app.mode = AppMode::RenameGroup;
                                        }
                                    }
                                }
                                KeyCode::Enter => {
                                    if let Some(g) = app.dashboard.groups.get(app.dashboard.selected).cloned() {
                                        if let Some(ref gdir) = app.group_dir.clone() {
                                            let group_file = if g.name == "default" {
                                                app.main_config_path.clone()
                                            } else {
                                                gdir.join(format!("{}.toml", g.name))
                                            };
                                            if let Ok(content) = std::fs::read_to_string(&group_file) {
                                                if let Ok(cfg) = toml::from_str::<ReloadableConfig>(&content) {
                                                    app.components.clear();
                                                    app.exclude_matchers.clear();
                                                    app.component_idx = None;
                                                    app.events.clear();
                                                    app.selected = 0;
                                                    for p in &cfg.proxy {
                                                        app.components.push(ComponentInfo {
                                                            name: p.name.clone(),
                                                            listen: p.listen.clone(),
                                                            exclude: None, include: None,
                                                        });
                                                    }
                                                    app.active_group = Some(g.name.clone());
                                                    app.config_path = group_file.clone();
                                                    if let Some(ref tx) = app.proxy_change_tx {
                                                        let _ = tx.send(ProxyChange::SwitchGroup(group_file));
                                                    }
                                                    app.mode = AppMode::Main;
                                                }
                                            }
                                        }
                                    }
                                }
                                KeyCode::Char(' ') => {
                                    if let Some(g) = app.dashboard.groups.get(app.dashboard.selected).cloned() {
                                        let group_file = if g.name == "default" {
                                            app.main_config_path.clone()
                                        } else if let Some(ref gdir) = app.group_dir {
                                            gdir.join(format!("{}.toml", g.name))
                                        } else {
                                            continue;
                                        };
                                        if let Ok(content) = std::fs::read_to_string(&group_file) {
                                            if let Ok(cfg) = toml::from_str::<ReloadableConfig>(&content) {
                                                app.dashboard.detail_group_name = g.name.clone();
                                                app.dashboard.detail_proxies = cfg.proxy.iter().map(|p| NewProxyEntry {
                                                    name: p.name.clone(),
                                                    protocol: p.protocol.clone(),
                                                    listen: p.listen.clone(),
                                                    remote: p.remote.clone(),
                                                    mode: p.mode.clone().unwrap_or_default(),
                                                    interface: p.interface.clone().unwrap_or_default(),
                                                }).collect();
                                                app.dashboard.detail_selected = 0;
                                                app.proxy_form = None;
                                                app.mode = AppMode::GroupDetail;
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        AppMode::NewGroupName => {
                            match key.code {
                                KeyCode::Esc => { app.mode = AppMode::Dashboard; }
                                KeyCode::Enter => {
                                    let name = app.dashboard.new_group_name.trim().to_string();
                                    if name.is_empty() {
                                        app.dashboard.error = Some("group name is required".into());
                                    } else if app.dashboard.groups.iter().any(|g| g.name == name) {
                                        app.dashboard.error = Some(format!("group \"{}\" already exists", name));
                                    } else {
                                        app.dashboard.error = None;
                                        app.mode = AppMode::NewGroupAddProxy;
                                    }
                                }
                                KeyCode::Backspace => { app.dashboard.new_group_name.pop(); app.dashboard.error = None; }
                                KeyCode::Char(c) => { app.dashboard.new_group_name.push(c); app.dashboard.error = None; }
                                _ => {}
                            }
                        }
                        AppMode::NewGroupAddProxy => {
                            // Reuse proxy form logic inline
                            if app.proxy_form.is_none() {
                                // Show proxy list with option to add more or finish
                                match key.code {
                                    KeyCode::Esc => {
                                        // Save group and go back to dashboard
                                        if let Some(ref gdir) = app.group_dir.clone() {
                                            let name = &app.dashboard.new_group_name;
                                            let file = gdir.join(format!("{}.toml", name));
                                            let mut content = String::new();
                                            for p in &app.dashboard.new_group_proxies {
                                                content.push_str(&format_proxy_toml(&p.name, &p.protocol, &p.listen, &p.remote, &p.mode, &p.interface));
                                            }
                                            let _ = std::fs::write(&file, content);
                                            app.dashboard = DashboardState::load(gdir, &app.main_config_path);
                                        }
                                        app.mode = AppMode::Dashboard;
                                    }
                                    KeyCode::Char('n') | KeyCode::Enter => {
                                        app.proxy_form = Some(ProxyForm::default());
                                    }
                                    _ => {}
                                }
                            } else {
                                // Proxy form active
                                if let Some(ref mut form) = app.proxy_form {
                                    match key.code {
                                        KeyCode::Esc => { app.proxy_form = None; }
                                        KeyCode::Tab => { let fc = form.row_count(); form.active_field = (form.active_field + 1) % fc; form.error = None; }
                                        KeyCode::BackTab => { let fc = form.row_count(); form.active_field = (form.active_field + fc - 1) % fc; form.error = None; }
                                        KeyCode::Enter => {
                                            let protocol = PROTOCOLS[form.protocol_idx];
                                            let name = form.fields[0].trim().to_string();
                                            let remote_host = if form.fields[3].is_empty() { "127.0.0.1" } else { form.fields[3].trim() };
                                            let remote_port = if form.fields[4].is_empty() { default_port(protocol) } else { form.fields[4].trim() };
                                            if name.is_empty() {
                                                form.error = Some("name is required".into());
                                            } else if app.dashboard.new_group_proxies.iter().any(|p| p.name == name) {
                                                form.error = Some(format!("name \"{}\" already exists", name));
                                            } else {
                                                let mode_str = MODES[form.mode_idx].to_string();
                                                let iface = form.fields[5].clone();
                                                let listen = if form.mode_idx == 1 { String::new() } else { auto_assign_listen_port(protocol) };
                                                let remote = format!("{}:{}", remote_host, remote_port);
                                                app.dashboard.new_group_proxies.push(NewProxyEntry {
                                                    name, protocol: protocol.to_string(), listen, remote, mode: mode_str, interface: iface,
                                                });
                                                app.proxy_form = None;
                                            }
                                        }
                                        KeyCode::Left if form.active_field == 1 => { form.protocol_idx = (form.protocol_idx + PROTOCOLS.len() - 1) % PROTOCOLS.len(); }
                                        KeyCode::Right if form.active_field == 1 => { form.protocol_idx = (form.protocol_idx + 1) % PROTOCOLS.len(); }
                                        KeyCode::Left if form.active_field == 2 => { form.mode_idx = (form.mode_idx + MODES.len() - 1) % MODES.len(); form.active_field = form.active_field.min(form.row_count() - 1); }
                                        KeyCode::Right if form.active_field == 2 => { form.mode_idx = (form.mode_idx + 1) % MODES.len(); form.active_field = form.active_field.min(form.row_count() - 1); }
                                        KeyCode::Backspace => {
                                            if form.active_field == 1 { form.protocol_idx = (form.protocol_idx + PROTOCOLS.len() - 1) % PROTOCOLS.len(); }
                                            else if form.active_field == 2 { form.mode_idx = (form.mode_idx + MODES.len() - 1) % MODES.len(); form.active_field = form.active_field.min(form.row_count() - 1); }
                                            else if let Some(fi) = form.field_idx() { form.fields[fi].pop(); }
                                            form.error = None;
                                        }
                                        KeyCode::Char(c) => {
                                            if form.active_field == 1 { form.protocol_idx = (form.protocol_idx + 1) % PROTOCOLS.len(); }
                                            else if form.active_field == 2 { form.mode_idx = (form.mode_idx + 1) % MODES.len(); form.active_field = form.active_field.min(form.row_count() - 1); }
                                            else if let Some(fi) = form.field_idx() { form.fields[fi].push(c); }
                                            form.error = None;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        AppMode::RenameGroup => {
                            match key.code {
                                KeyCode::Esc => { app.mode = AppMode::Dashboard; }
                                KeyCode::Enter => {
                                    let new_name = app.dashboard.rename_input.trim().to_string();
                                    if new_name.is_empty() {
                                        app.dashboard.error = Some("name is required".into());
                                    } else if app.dashboard.groups.iter().any(|g| g.name == new_name) {
                                        app.dashboard.error = Some(format!("\"{}\" already exists", new_name));
                                    } else if let Some(ref gdir) = app.group_dir.clone() {
                                        let old_name = &app.dashboard.groups[app.dashboard.selected].name;
                                        let old_file = gdir.join(format!("{}.toml", old_name));
                                        let new_file = gdir.join(format!("{}.toml", new_name));
                                        let _ = std::fs::rename(&old_file, &new_file);
                                        app.dashboard = DashboardState::load(gdir, &app.main_config_path);
                                        app.mode = AppMode::Dashboard;
                                    }
                                }
                                KeyCode::Backspace => { app.dashboard.rename_input.pop(); app.dashboard.error = None; }
                                KeyCode::Char(c) => { app.dashboard.rename_input.push(c); app.dashboard.error = None; }
                                _ => {}
                            }
                        }
                        AppMode::GroupDetail => {
                            if app.dashboard.detail_delete_confirm {
                                match key.code {
                                    KeyCode::Char('y') | KeyCode::Enter => {
                                        if app.dashboard.detail_selected < app.dashboard.detail_proxies.len() {
                                            app.dashboard.detail_proxies.remove(app.dashboard.detail_selected);
                                            if app.dashboard.detail_selected >= app.dashboard.detail_proxies.len() && !app.dashboard.detail_proxies.is_empty() {
                                                app.dashboard.detail_selected = app.dashboard.detail_proxies.len() - 1;
                                            }
                                            // Save to file
                                            let group_file = if app.dashboard.detail_group_name == "default" {
                                                app.main_config_path.clone()
                                            } else if let Some(ref gdir) = app.group_dir {
                                                gdir.join(format!("{}.toml", app.dashboard.detail_group_name))
                                            } else {
                                                app.dashboard.detail_delete_confirm = false;
                                                continue;
                                            };
                                            let mut content = String::new();
                                            for p in &app.dashboard.detail_proxies {
                                                content.push_str(&format_proxy_toml(&p.name, &p.protocol, &p.listen, &p.remote, &p.mode, &p.interface));
                                            }
                                            let _ = std::fs::write(&group_file, content);
                                            app.dashboard.detail_delete_confirm = false;
                                            continue;
                                        }
                                    }
                                    _ => {}
                                }
                                app.dashboard.detail_delete_confirm = false;
                                continue;
                            }
                            if let Some(ref mut form) = app.proxy_form {
                                match key.code {
                                    KeyCode::Esc => { app.proxy_form = None; }
                                    KeyCode::Tab => { let fc = form.row_count(); form.active_field = (form.active_field + 1) % fc; form.error = None; }
                                    KeyCode::BackTab => { let fc = form.row_count(); form.active_field = (form.active_field + fc - 1) % fc; form.error = None; }
                                    KeyCode::Enter => {
                                        let protocol = PROTOCOLS[form.protocol_idx];
                                        let name = form.fields[0].trim().to_string();
                                        let remote_host = if form.fields[3].is_empty() { "127.0.0.1" } else { form.fields[3].trim() };
                                        let remote_port = if form.fields[4].is_empty() { default_port(protocol) } else { form.fields[4].trim() };
                                        if name.is_empty() {
                                            form.error = Some("name is required".into());
                                        } else if app.dashboard.detail_proxies.iter().any(|p| p.name == name)
                                            && form.editing_idx.is_none() {
                                            form.error = Some(format!("name \"{}\" already exists", name));
                                        } else {
                                            let is_capture = form.mode_idx == 1;
                                            let listen = if is_capture {
                                                String::new()
                                            } else if form.editing_idx.is_some() && (!form.fields[1].is_empty() || !form.fields[2].is_empty()) {
                                                let lh = if form.fields[1].is_empty() { "127.0.0.1" } else { form.fields[1].trim() };
                                                let lp = if form.fields[2].is_empty() { "0" } else { form.fields[2].trim() };
                                                format!("{}:{}", lh, lp)
                                            } else {
                                                form.existing_listen.clone().unwrap_or_else(|| auto_assign_listen_port(protocol))
                                            };
                                            let remote = format!("{}:{}", remote_host, remote_port);
                                            let mode_str = MODES[form.mode_idx].to_string();
                                            let iface = form.fields[5].clone();
                                            if let Some(idx) = form.editing_idx {
                                                if idx < app.dashboard.detail_proxies.len() {
                                                    app.dashboard.detail_proxies[idx] = NewProxyEntry {
                                                        name, protocol: protocol.to_string(), listen, remote, mode: mode_str, interface: iface,
                                                    };
                                                }
                                            } else {
                                                app.dashboard.detail_proxies.push(NewProxyEntry {
                                                    name, protocol: protocol.to_string(), listen, remote, mode: mode_str, interface: iface,
                                                });
                                            }
                                            // Save to file
                                            let group_file = if app.dashboard.detail_group_name == "default" {
                                                app.main_config_path.clone()
                                            } else if let Some(ref gdir) = app.group_dir {
                                                gdir.join(format!("{}.toml", app.dashboard.detail_group_name))
                                            } else {
                                                app.proxy_form = None;
                                                continue;
                                            };
                                            let mut content = String::new();
                                            for p in &app.dashboard.detail_proxies {
                                                content.push_str(&format_proxy_toml(&p.name, &p.protocol, &p.listen, &p.remote, &p.mode, &p.interface));
                                            }
                                            let _ = std::fs::write(&group_file, content);
                                            app.proxy_form = None;
                                        }
                                    }
                                    KeyCode::Left if form.active_field == 1 => { form.protocol_idx = (form.protocol_idx + PROTOCOLS.len() - 1) % PROTOCOLS.len(); }
                                    KeyCode::Right if form.active_field == 1 => { form.protocol_idx = (form.protocol_idx + 1) % PROTOCOLS.len(); }
                                    KeyCode::Left if form.active_field == 2 => { form.mode_idx = (form.mode_idx + MODES.len() - 1) % MODES.len(); form.active_field = form.active_field.min(form.row_count() - 1); }
                                    KeyCode::Right if form.active_field == 2 => { form.mode_idx = (form.mode_idx + 1) % MODES.len(); form.active_field = form.active_field.min(form.row_count() - 1); }
                                    KeyCode::Backspace => {
                                        if form.active_field == 1 { form.protocol_idx = (form.protocol_idx + PROTOCOLS.len() - 1) % PROTOCOLS.len(); }
                                        else if form.active_field == 2 { form.mode_idx = (form.mode_idx + MODES.len() - 1) % MODES.len(); form.active_field = form.active_field.min(form.row_count() - 1); }
                                        else if let Some(fi) = form.field_idx() { form.fields[fi].pop(); }
                                        form.error = None;
                                    }
                                    KeyCode::Char(c) => {
                                        if form.active_field == 1 { form.protocol_idx = (form.protocol_idx + 1) % PROTOCOLS.len(); }
                                        else if form.active_field == 2 { form.mode_idx = (form.mode_idx + 1) % MODES.len(); form.active_field = form.active_field.min(form.row_count() - 1); }
                                        else if let Some(fi) = form.field_idx() { form.fields[fi].push(c); }
                                        form.error = None;
                                    }
                                    _ => {}
                                }
                            } else {
                                match key.code {
                                    KeyCode::Esc => {
                                        if let Some(ref gdir) = app.group_dir {
                                            app.dashboard = DashboardState::load(gdir, &app.main_config_path);
                                        }
                                        app.mode = AppMode::Dashboard;
                                    }
                                    KeyCode::Char('j') | KeyCode::Down => {
                                        if app.dashboard.detail_selected + 1 < app.dashboard.detail_proxies.len() {
                                            app.dashboard.detail_selected += 1;
                                        }
                                    }
                                    KeyCode::Char('k') | KeyCode::Up => {
                                        app.dashboard.detail_selected = app.dashboard.detail_selected.saturating_sub(1);
                                    }
                                    KeyCode::Char('n') => {
                                        app.proxy_form = Some(ProxyForm::default());
                                    }
                                    KeyCode::Char('e') => {
                                        if let Some(entry) = app.dashboard.detail_proxies.get(app.dashboard.detail_selected) {
                                            let mut form = ProxyForm::from_entry(entry);
                                            form.editing_idx = Some(app.dashboard.detail_selected);
                                            app.proxy_form = Some(form);
                                        }
                                    }
                                    KeyCode::Char('d') => {
                                        if !app.dashboard.detail_proxies.is_empty() {
                                            app.dashboard.detail_delete_confirm = true;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        AppMode::Main => unreachable!(),
                    }
                }
            }
            // Drain event receiver while in dashboard so stale events don't accumulate
            while rx.try_recv().is_ok() {}
            continue;
        }

        // === Main TUI mode below ===
        // Hot-reload config on file change
        if let Ok(meta) = std::fs::metadata(&config_path) {
            if let Ok(mtime) = meta.modified() {
                if mtime != last_mtime {
                    last_mtime = mtime;
                    if let Ok(content) = std::fs::read_to_string(&config_path) {
                        if let Ok(cfg) = toml::from_str::<ReloadableConfig>(&content) {
                            reload_config(&mut app, &cfg);
                        }
                    }
                }
            }
        }

        // Handle proxy hot-reload notifications
        if let Some(ref mut prx) = proxy_change_rx {
            while let Ok(change) = prx.try_recv() {
                match change {
                    ProxyChange::Added(ci) => {
                        if !app.components.iter().any(|c| c.name == ci.name) {
                            let matcher = ExcludeMatcher::new(ci.exclude.as_ref(), ci.include.as_ref());
                            if matcher.excludes.is_empty() && matcher.includes.is_empty() {
                            } else {
                                app.exclude_matchers.insert(ci.name.clone(), matcher);
                            }
                            app.components.push(ci);
                        }
                    }
                    ProxyChange::Removed(name) => {
                        app.components.retain(|c| c.name != name);
                        app.exclude_matchers.remove(&name);
                        if let Some(idx) = app.component_idx {
                            if idx >= app.components.len() {
                                app.component_idx = None;
                            }
                        }
                    }
                    ProxyChange::SwitchGroup(_) | ProxyChange::StopAll => {} // handled by main.rs watcher
                }
            }
        }

        while let Ok(ev) = rx.try_recv() {
            // System events bypass component/exclude filters
            if !ev.system {
                // Only accept events from components in the current group
                if !app.components.iter().any(|c| c.name == ev.component) { continue; }
                if let Some(matcher) = app.exclude_matchers.get(&ev.component) {
                    if matcher.is_excluded(&ev.command) { continue; }
                }
            }
            if app.paused {
                app.component_stats.entry(ev.component.clone()).or_default().record(&ev);
                app.paused_buffer.push(ev);
            } else {
                app.component_stats.entry(ev.component.clone()).or_default().record(&ev);
                app.events.push(ev);
                app.dirty = true;
                if app.follow && app.focus == Focus::Events && app.filter.is_empty() {
                    app.refresh_filter_cache();
                    app.selected = app.cached_filtered_indices.len().saturating_sub(1);
                }
            }
        }

        if app.dirty {
            terminal.draw(|f| ui(f, &mut app))?;
            app.dirty = false;
        }

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press { continue; }

                // Ctrl+C: force quit regardless of state
                if key.code == KeyCode::Char('c') && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
                    break;
                }

                // Group picker handling
                if let Some(ref mut picker) = app.group_picker {
                    match key.code {
                        KeyCode::Esc => { app.group_picker = None; }
                        KeyCode::Char('j') | KeyCode::Down
                            if picker.selected + 1 < picker.groups.len() => { picker.selected += 1; }
                        KeyCode::Char('k') | KeyCode::Up => {
                            picker.selected = picker.selected.saturating_sub(1);
                        }
                        KeyCode::Enter => {
                            let group_name = picker.groups[picker.selected].clone();
                            app.group_picker = None;
                            if let Some(ref gdir) = app.group_dir {
                                // "default" maps to main config (parent of group dir)
                                let group_file = if group_name == "default" {
                                    gdir.parent().unwrap_or(gdir.as_path()).join("ocular.toml")
                                } else {
                                    gdir.join(format!("{}.toml", group_name))
                                };
                                if group_file.exists() {
                                    if let Ok(content) = std::fs::read_to_string(&group_file) {
                                        if let Ok(cfg) = toml::from_str::<ReloadableConfig>(&content) {
                                            app.components.clear();
                                            app.exclude_matchers.clear();
                                            app.component_idx = None;
                                            app.events.clear();
                                            app.selected = 0;
                                            for p in &cfg.proxy {
                                                let ci = ComponentInfo {
                                                    name: p.name.clone(),
                                                    listen: p.listen.clone(),
                                                    exclude: None,
                                                    include: None,
                                                };
                                                app.components.push(ci);
                                            }
                                            app.active_group = Some(group_name);
                                            app.config_path = group_file.clone();
                                            if let Some(ref tx) = app.proxy_change_tx {
                                                let _ = tx.send(ProxyChange::SwitchGroup(group_file));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                // Proxy form handling
                if let Some(ref mut form) = app.proxy_form {
                    let field_count: usize = form.row_count();
                    match key.code {
                        KeyCode::Esc => { app.proxy_form = None; }
                        KeyCode::Tab => { form.active_field = (form.active_field + 1) % field_count; form.error = None; }
                        KeyCode::BackTab => { form.active_field = (form.active_field + field_count - 1) % field_count; form.error = None; }
                        KeyCode::Enter => {
                            let protocol = PROTOCOLS[form.protocol_idx];
                            let name = form.fields[0].trim().to_string();
                            let remote_host = if form.fields[3].is_empty() { "127.0.0.1" } else { form.fields[3].trim() };
                            let remote_port = if form.fields[4].is_empty() { default_port(protocol) } else { form.fields[4].trim() };

                            // Validation
                            if name.is_empty() {
                                form.error = Some("name is required".into());
                            } else if remote_port.is_empty() {
                                form.error = Some("remote port is required".into());
                            } else {
                                // Name uniqueness check
                                let name_taken = app.components.iter().enumerate().any(|(i, c)| {
                                    c.name == name && form.editing_idx != Some(i)
                                });
                                if name_taken {
                                    form.error = Some(format!("name \"{}\" already exists", name));
                                } else {
                                    let is_capture = form.mode_idx == 1;
                                    if is_capture && form.fields[5].trim().is_empty() {
                                        form.error = Some("interface is required for capture mode".into());
                                    } else {
                                    let listen_addr = if is_capture {
                                        String::new()
                                    } else if form.editing_idx.is_some() && (!form.fields[1].is_empty() || !form.fields[2].is_empty()) {
                                        let lh = if form.fields[1].is_empty() { "127.0.0.1" } else { form.fields[1].trim() };
                                        let lp = if form.fields[2].is_empty() { "0" } else { form.fields[2].trim() };
                                        format!("{}:{}", lh, lp)
                                    } else {
                                        form.existing_listen.clone().unwrap_or_else(|| auto_assign_listen_port(protocol))
                                    };
                                    let mode_str = MODES[form.mode_idx];
                                    let iface = form.fields[5].trim().to_string();
                                    {
                                        let remote_addr = format!("{}:{}", remote_host, remote_port);
                                        let editing_idx = form.editing_idx;
                                        app.proxy_form = None;
                                        let ci = ComponentInfo {
                                            name: name.clone(),
                                            listen: listen_addr.clone(),
                                            exclude: None,
                                            include: None,
                                        };
                                        if let Some(idx) = editing_idx {
                                            if let Some(c) = app.components.get_mut(idx) {
                                                c.name = name.clone();
                                                c.listen = listen_addr.clone();
                                            }
                                        } else {
                                            app.components.push(ci);
                                        }
                                        save_proxy_config(&app.config_path, &app.components, protocol, editing_idx, &name, &listen_addr, &remote_addr, mode_str, &iface);
                                    }
                                    }
                                }
                            }
                        }
                        KeyCode::Left if form.active_field == 1 => {
                            form.protocol_idx = (form.protocol_idx + PROTOCOLS.len() - 1) % PROTOCOLS.len();
                        }
                        KeyCode::Right if form.active_field == 1 => {
                            form.protocol_idx = (form.protocol_idx + 1) % PROTOCOLS.len();
                        }
                        KeyCode::Left if form.active_field == 2 => {
                            form.mode_idx = (form.mode_idx + MODES.len() - 1) % MODES.len();
                            form.active_field = form.active_field.min(form.row_count() - 1);
                        }
                        KeyCode::Right if form.active_field == 2 => {
                            form.mode_idx = (form.mode_idx + 1) % MODES.len();
                            form.active_field = form.active_field.min(form.row_count() - 1);
                        }
                        KeyCode::Backspace => {
                            if form.active_field == 1 {
                                form.protocol_idx = (form.protocol_idx + PROTOCOLS.len() - 1) % PROTOCOLS.len();
                            } else if form.active_field == 2 {
                                form.mode_idx = (form.mode_idx + MODES.len() - 1) % MODES.len();
                                form.active_field = form.active_field.min(form.row_count() - 1);
                            } else if let Some(fi) = form.field_idx() {
                                form.fields[fi].pop();
                            }
                            form.error = None;
                        }
                        KeyCode::Char(c) => {
                            if form.active_field == 1 {
                                form.protocol_idx = (form.protocol_idx + 1) % PROTOCOLS.len();
                            } else if form.active_field == 2 {
                                form.mode_idx = (form.mode_idx + 1) % MODES.len();
                                form.active_field = form.active_field.min(form.row_count() - 1);
                            } else if let Some(fi) = form.field_idx() {
                                form.fields[fi].push(c);
                            }
                            form.error = None;
                        }
                        _ => {}
                    }
                    continue;
                }

                // Delete confirm handling
                if let Some(idx) = app.delete_confirm_idx {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Enter => {
                            if idx < app.components.len() {
                                let removed = app.components.remove(idx);
                                app.exclude_matchers.remove(&removed.name);
                                delete_proxy_from_config(&app.config_path, &removed.name);
                                if let Some(ci) = app.component_idx {
                                    if ci >= app.components.len() {
                                        app.component_idx = if app.components.is_empty() { None } else { Some(app.components.len() - 1) };
                                    }
                                }
                            }
                            app.delete_confirm_idx = None;
                        }
                        _ => { app.delete_confirm_idx = None; }
                    }
                    continue;
                }

                // Info popup handling
                if app.info_popup_idx.is_some() {
                    match key.code {
                        KeyCode::Char('i') | KeyCode::Esc => { app.info_popup_idx = None; }
                        _ => {}
                    }
                    continue;
                }

                // Component filter handling
                if app.focus == Focus::ComponentFilter {
                    match key.code {
                        KeyCode::Esc => {
                            app.component_filter.clear();
                            app.focus = Focus::Components;
                            app.dirty = true;
                        }
                        KeyCode::Enter => { app.focus = Focus::Components; app.dirty = true; }
                        KeyCode::Backspace => { app.component_filter.pop(); app.dirty = true; }
                        KeyCode::Char(c) => { app.component_filter.push(c); app.dirty = true; }
                        _ => {}
                    }
                    continue;
                }

                if app.focus == Focus::Filter {
                    match key.code {
                        KeyCode::Esc => { app.focus = Focus::Events; app.dirty = true; }
                        KeyCode::Enter => { app.focus = Focus::Events; app.selected = 0; app.dirty = true; }
                        KeyCode::Backspace => { app.filter.pop(); app.selected = 0; app.dirty = true; }
                        KeyCode::Char(c) => { app.filter.push(c); app.selected = 0; app.dirty = true; }
                        _ => {}
                    }
                    continue;
                }

                if app.help_active {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('?') => { app.help_active = false; }
                        _ => {}
                    }
                    continue;
                }

                if app.confirm_quit {
                    match key.code {
                        KeyCode::Char('y') => {
                            if let Some(ref tx) = app.proxy_change_tx {
                                let _ = tx.send(ProxyChange::StopAll);
                            }
                            app.mode = AppMode::Dashboard;
                            app.confirm_quit = false;
                            if let Some(ref gdir) = app.group_dir.clone() {
                                app.dashboard = DashboardState::load(gdir, &app.main_config_path);
                            }
                        }
                        _ => { app.confirm_quit = false; }
                    }
                    continue;
                }

                if app.leader_active {
                    app.leader_active = false;
                    match key.code {
                        KeyCode::Char('j') => { app.focus = Focus::Detail; app.detail_scroll = 0; }
                        KeyCode::Char('k') => { app.focus = Focus::Events; }
                        KeyCode::Char('h') => { app.focus = Focus::Components; }
                        KeyCode::Char('l') => { app.focus = Focus::Events; }
                        KeyCode::Char('c') => { app.events.clear(); app.selected = 0; app.dirty = true; }
                        KeyCode::Char('f') => { app.follow = !app.follow; }
                        KeyCode::Char('p') => {
                            app.paused = !app.paused;
                            if !app.paused && !app.paused_buffer.is_empty() {
                                app.events.append(&mut app.paused_buffer);
                                app.dirty = true;
                                app.refresh_filter_cache();
                                app.selected = app.cached_filtered_indices.len().saturating_sub(1);
                            }
                        }
                        KeyCode::Char(',') => {
                            // Open config in $EDITOR
                            disable_raw_mode()?;
                            stdout().execute(LeaveAlternateScreen)?;
                            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
                            let _ = std::process::Command::new(&editor)
                                .arg(&config_path)
                                .status();
                            stdout().execute(EnterAlternateScreen)?;
                            enable_raw_mode()?;
                            terminal.clear()?;
                        }
                        KeyCode::Char('g') => {
                            // Open group picker
                            if let Some(ref gdir) = app.group_dir {
                                let mut groups: Vec<String> = vec!["default".to_string()];
                                if let Ok(entries) = std::fs::read_dir(gdir) {
                                    for entry in entries.flatten() {
                                        let path = entry.path();
                                        if path.extension().is_some_and(|e| e == "toml") {
                                            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                                                groups.push(stem.to_string());
                                            }
                                        }
                                    }
                                }
                                groups[1..].sort();
                                let selected = app.active_group.as_ref()
                                    .and_then(|ag| groups.iter().position(|g| g == ag))
                                    .unwrap_or(0);
                                app.group_picker = Some(GroupPicker { groups, selected });
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => {
                        if app.quit_confirm_enabled { app.confirm_quit = true; } else {
                            // Stop all proxies before returning to dashboard
                            if let Some(ref tx) = app.proxy_change_tx {
                                let _ = tx.send(ProxyChange::StopAll);
                            }
                            app.mode = AppMode::Dashboard;
                            if let Some(ref gdir) = app.group_dir.clone() {
                                app.dashboard = DashboardState::load(gdir, &app.main_config_path);
                            }
                        }
                    }
                    KeyCode::Char('?') => { app.help_active = !app.help_active; }
                    KeyCode::Char(' ') => {
                        app.pending_keys.clear();
                        app.leader_active = true;
                    }
                    KeyCode::Char('/') => {
                        app.pending_keys.clear();
                        if app.focus == Focus::Components {
                            app.focus = Focus::ComponentFilter;
                        } else {
                            app.focus = Focus::Filter;
                        }
                    }
                    KeyCode::Esc => {
                        app.pending_keys.clear();
                        if app.focus == Focus::Detail {
                            app.focus = Focus::Events;
                        } else if app.focus == Focus::Components && !app.component_filter.is_empty() {
                            app.component_filter.clear();
                            app.dirty = true;
                        } else if !app.filter.is_empty() {
                            app.filter.clear();
                            app.selected = 0;
                            app.focus = Focus::Events;
                            app.dirty = true;
                        } else if app.visual_mode {
                            app.visual_mode = false;
                        } else {
                            app.component_idx = None;
                            app.selected = 0;
                            app.focus = Focus::Events;
                            app.dirty = true;
                        }
                    }
                    KeyCode::Tab => {
                        app.pending_keys.clear();
                        app.focus = match app.focus {
                            Focus::Components | Focus::ComponentFilter => Focus::Events,
                            Focus::Events => Focus::Detail,
                            Focus::Detail => Focus::Components,
                            Focus::Filter => Focus::Events,
                        };
                        app.detail_scroll = 0;
                    }
                    KeyCode::BackTab => {
                        app.pending_keys.clear();
                        app.focus = match app.focus {
                            Focus::Components | Focus::ComponentFilter => Focus::Detail,
                            Focus::Events => Focus::Components,
                            Focus::Detail => Focus::Events,
                            Focus::Filter => Focus::Events,
                        };
                        app.detail_scroll = 0;
                    }
                    KeyCode::Char('h') => { app.focus = Focus::Components; app.detail_scroll = 0; }
                    KeyCode::Char('l') => {
                        app.focus = if app.focus == Focus::Components { Focus::Events } else { Focus::Detail };
                        app.detail_scroll = 0;
                    }
                    KeyCode::Char('G') if app.focus == Focus::Events => {
                        app.pending_keys.clear();
                        app.refresh_filter_cache();
                        let max = app.cached_filtered_indices.len().saturating_sub(1);
                        app.selected = max;
                        app.detail_scroll = 0;
                        app.follow = true;
                    }
                    KeyCode::Char('G') if app.focus == Focus::Detail => {
                        app.pending_keys.clear();
                        app.detail_scroll = u16::MAX;
                    }
                    KeyCode::Char('g') if app.focus == Focus::Events => {
                        if app.pending_keys.ends_with('g') {
                            let num_str: String = app.pending_keys.chars().take_while(|c| c.is_ascii_digit()).collect();
                            app.refresh_filter_cache();
                            let max = app.cached_filtered_indices.len().saturating_sub(1);
                            if num_str.is_empty() {
                                app.selected = 0;
                            } else if let Ok(n) = num_str.parse::<usize>() {
                                app.selected = n.saturating_sub(1).min(max);
                            }
                            app.pending_keys.clear();
                            app.detail_scroll = 0;
                            app.follow = false;
                        } else {
                            app.pending_keys.push('g');
                        }
                    }
                    KeyCode::Char('g') if app.focus == Focus::Detail => {
                        if app.pending_keys.ends_with('g') {
                            app.detail_scroll = 0;
                            app.pending_keys.clear();
                        } else {
                            app.pending_keys.push('g');
                        }
                    }
                    KeyCode::Char(c @ '0'..='9') if app.focus == Focus::Events => {
                        app.pending_keys.push(c);
                    }
                    KeyCode::Char('y') if app.focus == Focus::Events || app.focus == Focus::Detail => {
                        app.pending_keys.clear();
                        let filtered = app.filtered_events();
                        let text = get_selected_commands(&filtered, &app);
                        if !text.is_empty() {
                            copy_to_clipboard(&text);
                        }
                        app.visual_mode = false;
                    }
                    KeyCode::Char('v') if app.focus == Focus::Events => {
                        app.pending_keys.clear();
                        if app.visual_mode {
                            app.visual_mode = false;
                        } else {
                            app.visual_mode = true;
                            app.visual_anchor = app.selected;
                        }
                    }
                    KeyCode::Char('e') if app.focus == Focus::Events => {
                        app.pending_keys.clear();
                        let filtered = app.filtered_events();
                        let text = get_selected_commands(&filtered, &app);
                        if !text.is_empty() {
                            disable_raw_mode()?;
                            stdout().execute(LeaveAlternateScreen)?;
                            open_in_editor(&text);
                            stdout().execute(EnterAlternateScreen)?;
                            enable_raw_mode()?;
                            terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
                        }
                        app.visual_mode = false;
                    }
                    KeyCode::Char('e') if app.focus == Focus::Detail => {
                        app.pending_keys.clear();
                        let filtered = app.filtered_events();
                        if let Some((_, ev, _)) = filtered.get(app.selected) {
                            let meta = format!("# Time: {}  Src: {}  Dest: {}  Process: {}  Latency: {}",
                                format_time(&ev.timestamp),
                                ev.src.as_deref().unwrap_or("-"),
                                ev.dest.as_deref().unwrap_or("-"),
                                ev.process.as_deref().unwrap_or("-"),
                                format_latency(&ev.latency));
                            let detail_content = format!("{}\n\n{}\n\n{}",
                                ev.full_command, ev.response_detail, meta);
                            disable_raw_mode()?;
                            stdout().execute(LeaveAlternateScreen)?;
                            open_in_editor(&detail_content);
                            stdout().execute(EnterAlternateScreen)?;
                            enable_raw_mode()?;
                            terminal = Terminal::new(CrosstermBackend::new(stdout()))?;
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.pending_keys.clear();
                        match app.focus {
                            Focus::Components => {
                                let visible = filtered_component_indices(&app);
                                if visible.is_empty() {
                                    app.component_idx = None;
                                } else {
                                    let cur_pos = app.component_idx.and_then(|ci| visible.iter().position(|&v| v == ci));
                                    app.component_idx = match cur_pos {
                                        None => Some(*visible.last().unwrap()),
                                        Some(0) => if app.component_filter.is_empty() { None } else { Some(visible[0]) },
                                        Some(p) => Some(visible[p - 1]),
                                    };
                                }
                                app.selected = 0;
                            }
                            Focus::Events => { app.selected = app.selected.saturating_sub(1); app.detail_scroll = 0; app.follow = false; }
                            Focus::Detail => { app.detail_scroll = app.detail_scroll.saturating_sub(1); }
                            _ => {}
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.pending_keys.clear();
                        match app.focus {
                            Focus::Components => {
                                let visible = filtered_component_indices(&app);
                                if visible.is_empty() {
                                    app.component_idx = None;
                                } else {
                                    let cur_pos = app.component_idx.and_then(|ci| visible.iter().position(|&v| v == ci));
                                    app.component_idx = match cur_pos {
                                        None => Some(visible[0]),
                                        Some(p) if p + 1 < visible.len() => Some(visible[p + 1]),
                                        _ => if app.component_filter.is_empty() { None } else { app.component_idx },
                                    };
                                }
                                app.selected = 0;
                            }
                            Focus::Events => {
                                app.refresh_filter_cache();
                                let max = app.cached_filtered_indices.len().saturating_sub(1);
                                if app.selected < max {
                                    app.selected += 1;
                                    app.detail_scroll = 0;
                                }
                            }
                            Focus::Detail => { app.detail_scroll += 1; }
                            _ => {}
                        }
                    }
                    KeyCode::Enter => {
                        app.pending_keys.clear();
                        if app.focus == Focus::Components {
                            app.focus = Focus::Events;
                            app.selected = 0;
                        } else if app.focus == Focus::Events {
                            app.focus = Focus::Detail;
                            app.detail_scroll = 0;
                        }
                    }
                    KeyCode::Char('n') if app.focus == Focus::Components => {
                        app.pending_keys.clear();
                        app.proxy_form = Some(ProxyForm::default());
                    }
                    KeyCode::Char('e') if app.focus == Focus::Components => {
                        app.pending_keys.clear();
                        if let Some(idx) = app.component_idx {
                            if let Some(ci) = app.components.get(idx) {
                                let mut form = ProxyForm {
                                    fields: [ci.name.clone(), String::new(), String::new(), String::new(), String::new(), String::new()],
                                    active_field: 0,
                                    editing_idx: Some(idx),
                                    protocol_idx: 0,
                                    mode_idx: 0,
                                    error: None,
                                    existing_listen: Some(ci.listen.clone()),
                                };
                                if let Ok(content) = std::fs::read_to_string(&app.config_path) {
                                    if let Ok(cfg) = toml::from_str::<ReloadableConfig>(&content) {
                                        if let Some(p) = cfg.proxy.iter().find(|p| p.name == ci.name) {
                                            form.protocol_idx = PROTOCOLS.iter().position(|&x| x == p.protocol).unwrap_or(0);
                                            form.mode_idx = if p.mode.as_deref() == Some("capture") { 1 } else { 0 };
                                            let (rh, rp) = split_addr(&p.remote);
                                            form.fields[3] = rh;
                                            form.fields[4] = rp;
                                            let (lh, lp) = split_addr(&ci.listen);
                                            form.fields[1] = lh;
                                            form.fields[2] = lp;
                                            form.fields[5] = p.interface.clone().unwrap_or_default();
                                        }
                                    }
                                }
                                app.proxy_form = Some(form);
                            }
                        }
                    }
                    KeyCode::Char('d') if app.focus == Focus::Components => {
                        app.pending_keys.clear();
                        if let Some(idx) = app.component_idx {
                            app.delete_confirm_idx = Some(idx);
                        }
                    }
                    KeyCode::Char('i') if app.focus == Focus::Components => {
                        app.pending_keys.clear();
                        if let Some(idx) = app.component_idx {
                            app.info_popup_idx = Some(idx);
                        }
                    }
                    _ => { app.pending_keys.clear(); }
                }
                app.dirty = true;
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn default_true() -> bool { true }

fn format_time(ts: &std::time::SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = (*ts).into();
    dt.format("%H:%M:%S%.3f").to_string()
}

fn format_sql(sql: &str) -> String {
    sqlformat::format(sql, &sqlformat::QueryParams::None, sqlformat::FormatOptions {
        indent: sqlformat::Indent::Spaces(2),
        uppercase: true,
        lines_between_queries: 1,
    })
}

fn format_latency(d: &Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms >= 1000.0 {
        format!("{}ms", ms as u64)
    } else if ms >= 100.0 {
        format!("{:.1}ms", ms)
    } else if ms >= 10.0 {
        format!("{:.2}ms", ms)
    } else {
        format!("{:.3}ms", ms)
    }
}

fn reload_config(app: &mut App, cfg: &ReloadableConfig) {
    // Rebuild exclude matchers
    let mut new_matchers = std::collections::HashMap::new();
    for proxy in &cfg.proxy {
        let global = cfg.exclude.get(&proxy.protocol);
        let local = proxy.exclude.as_ref();
        let include = proxy.include.as_ref();

        let exclude_cfgs: Option<Vec<ExcludeConfig>> = match (global, local) {
            (Some(g), Some(l)) => Some(vec![
                ExcludeConfig { patterns: g.patterns.clone(), case_sensitive: g.case_sensitive, regex: g.regex },
                ExcludeConfig { patterns: l.patterns.clone(), case_sensitive: l.case_sensitive, regex: l.regex },
            ]),
            (Some(g), None) => Some(vec![
                ExcludeConfig { patterns: g.patterns.clone(), case_sensitive: g.case_sensitive, regex: g.regex },
            ]),
            (None, Some(l)) => Some(vec![
                ExcludeConfig { patterns: l.patterns.clone(), case_sensitive: l.case_sensitive, regex: l.regex },
            ]),
            (None, None) => None,
        };
        let include_cfg = include.map(|i| ExcludeConfig {
            patterns: i.patterns.clone(), case_sensitive: i.case_sensitive, regex: i.regex,
        });

        if exclude_cfgs.is_some() || include_cfg.is_some() {
            new_matchers.insert(proxy.name.clone(), ExcludeMatcher::new(exclude_cfgs.as_ref(), include_cfg.as_ref()));
        }
    }
    app.exclude_matchers = new_matchers;

    // Rebuild theme
    let base = Theme::by_name(cfg.theme.as_deref().unwrap_or("default"));
    app.theme = if let Some(ref overrides) = cfg.theme_overrides {
        Theme::from_config(overrides, &base)
    } else {
        base
    };

    // Reload event format
    app.event_format = cfg.event_format.as_deref()
        .map(EventFormat::parse)
        .unwrap_or_else(EventFormat::default_format);

    // Reload latency threshold
    app.latency_threshold_ms = cfg.latency_threshold_ms;

    // Reload fuzzy filter setting
    app.fuzzy_filter = cfg.fuzzy_filter;
}

fn ui_dashboard(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(area);

    let main_area = chunks[0];
    let box_w: u16 = 52;

    match &app.mode {
        AppMode::Dashboard => {
            let mut lines: Vec<Line> = Vec::new();

            let visible = app.dashboard.filtered_indices();
            let max_visible: usize = 10;
            let selected_pos = visible.iter().position(|&i| i == app.dashboard.selected).unwrap_or(0);
            let scroll_start = if selected_pos < max_visible {
                0
            } else {
                selected_pos - max_visible + 1
            };
            let visible_window = &visible[scroll_start..visible.len().min(scroll_start + max_visible)];

            for &i in visible_window {
                let g = &app.dashboard.groups[i];
                let is_selected = i == app.dashboard.selected;
                let prefix = if is_selected { " ● " } else { "   " };
                let proxies_str = if g.proxies.is_empty() {
                    "(empty)".to_string()
                } else {
                    let s = g.proxies.join(", ");
                    if s.len() > 28 { format!("{}...", &s[..25]) } else { s }
                };
                let name_style = if is_selected {
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, if is_selected { Style::default().fg(Color::Cyan) } else { Style::default() }),
                    Span::styled(format!("{:<10}", g.name), name_style),
                    Span::styled(format!(" [{}]", proxies_str), Style::default().fg(Color::DarkGray)),
                ]));
            }
            // Scroll indicator
            if visible.len() > max_visible {
                let indicator = format!(" ({}/{})", selected_pos + 1, visible.len());
                lines.push(Line::from(Span::styled(indicator, Style::default().fg(Color::DarkGray))));
            }

            if !app.dashboard.filter.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(" (filter: {})", app.dashboard.filter),
                    Style::default().fg(Color::Rgb(255, 165, 0)),
                )));
            }
            lines.push(Line::from(""));

            let box_h = lines.len() as u16 + 2;
            let art_h: u16 = 4; // 3 lines ASCII art + 1 line version
            let gap: u16 = 1;
            let x = (main_area.width.saturating_sub(box_w)) / 2;
            let y = (main_area.height.saturating_sub(art_h + gap + box_h)) / 2 + art_h + gap;
            let box_area = Rect::new(x, y, box_w, box_h);
            let block = Block::default().borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray))
                .title_top(Line::from(vec![Span::styled(" Groups ", Style::default().fg(Color::DarkGray))]));
            let content = Paragraph::new(lines).block(block);
            f.render_widget(content, box_area);

            // Render ASCII art title above box
            let ascii_art = [
                "▄▖    ▜     ",
                "▌▌▛▘▌▌▐ ▀▌▛▘",
                "▙▌▙▖▙▌▐▖█▌▌ ",
            ];

            let mut art_lines: Vec<Line> = ascii_art.iter().map(|s| {
                Line::from(Span::styled(*s, Style::default().fg(Color::Cyan))).centered()
            }).collect();
            art_lines.push(
                Line::from(Span::styled(
                    format!("v{}", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(Color::DarkGray),
                )).centered(),
            );
            let art_area = Rect::new(x, y - gap - art_h, box_w, art_h);
            f.render_widget(Paragraph::new(art_lines), art_area);

            // Filter input at bottom of box
            if app.dashboard.filter_active {
                let filter_area = Rect::new(x, y + box_h, box_w, 1);
                let filter_line = Paragraph::new(Line::from(Span::styled(
                    format!(" /{}", app.dashboard.filter),
                    Style::default().fg(Color::Rgb(255, 165, 0)),
                )));
                f.render_widget(filter_line, filter_area);
            }

            // Status bar
            let key_style = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
            let sep = Style::default().fg(Color::DarkGray);
            let status = if app.dashboard.filter_active {
                Line::from(Span::styled(format!(" /{}", app.dashboard.filter), Style::default().fg(Color::Rgb(255, 165, 0))))
            } else {
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled("n", key_style), Span::raw(" new "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("r", key_style), Span::raw(" rename "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("e", key_style), Span::raw(" edit "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("d", key_style), Span::raw(" delete "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("/", key_style), Span::raw(" filter "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("Space", key_style), Span::raw(" detail "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("↵", key_style), Span::raw(" load "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("q", key_style), Span::raw(" quit"),
                ]).centered()
            };
            let status_block = Block::default().borders(Borders::TOP | Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray));
            let status_inner = status_block.inner(chunks[1]);
            f.render_widget(status_block, chunks[1]);
            f.render_widget(Paragraph::new(status), status_inner);
            // Delete confirm popup
            if app.dashboard.delete_confirm {
                if let Some(g) = app.dashboard.groups.get(app.dashboard.selected) {
                    let w: u16 = 36;
                    let h: u16 = 5;
                    let x = (area.width.saturating_sub(w)) / 2;
                    let y = (area.height.saturating_sub(h)) / 2;
                    let popup_area = Rect::new(x, y, w, h);
                    f.render_widget(Clear, popup_area);
                    let lines = vec![
                        Line::from(format!(" Delete \"{}\"?", g.name)),
                        Line::from(""),
                        Line::from(vec![
                            Span::raw(" "),
                            Span::styled("y/Enter", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                            Span::raw(" confirm  "),
                            Span::styled("n/Esc", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                            Span::raw(" cancel"),
                        ]),
                    ];
                    let popup = Paragraph::new(lines).block(Block::default().borders(Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded)
                        .border_style(Style::default().fg(Color::Yellow))
                        .title(" Confirm "));
                    f.render_widget(popup, popup_area);
                }
            }
        }
        AppMode::NewGroupName | AppMode::RenameGroup => {
            let is_rename = app.mode == AppMode::RenameGroup;
            let title = if is_rename { " Rename Group " } else { " New Group " };
            let input = if is_rename { &app.dashboard.rename_input } else { &app.dashboard.new_group_name };

            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(" Group name: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}▌", input), Style::default().fg(Color::White)),
            ]));
            if let Some(ref err) = app.dashboard.error {
                lines.push(Line::from(Span::styled(format!(" ⚠ {}", err), Style::default().fg(Color::Red))));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(" Enter: confirm  Esc: cancel", Style::default().fg(Color::DarkGray))));

            let box_h = lines.len() as u16 + 2;
            let x = (main_area.width.saturating_sub(box_w)) / 2;
            let y = (main_area.height.saturating_sub(box_h)) / 2;
            let box_area = Rect::new(x, y, box_w, box_h);
            let block = Block::default().borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title);
            f.render_widget(Paragraph::new(lines).block(block), box_area);

            if !is_rename && y + box_h < main_area.height {
                let hint_area = Rect::new(x, y + box_h + 1, box_w, 1);
                let hint = Paragraph::new(Line::from(Span::styled(
                    "Create a group for organizing proxies",
                    Style::default().fg(Color::Rgb(80, 80, 80)),
                ))).centered();
                f.render_widget(hint, hint_area);
            }
        }
        AppMode::NewGroupAddProxy => {
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(format!(" Group: {}", app.dashboard.new_group_name), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))));
            lines.push(Line::from(""));

            if app.dashboard.new_group_proxies.is_empty() {
                lines.push(Line::from(Span::styled(" No proxies yet", Style::default().fg(Color::DarkGray))));
            } else {
                for p in &app.dashboard.new_group_proxies {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {} ", p.name), Style::default().fg(Color::White)),
                        Span::styled(format!("({} → {})", p.listen, p.remote), Style::default().fg(Color::DarkGray)),
                    ]));
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(" n/Enter: add proxy  Esc: save & back", Style::default().fg(Color::DarkGray))));

            let box_h = lines.len() as u16 + 2;
            let x = (main_area.width.saturating_sub(box_w)) / 2;
            let y = (main_area.height.saturating_sub(box_h)) / 2;
            let box_area = Rect::new(x, y, box_w, box_h);
            let block = Block::default().borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" New Group ");
            f.render_widget(Paragraph::new(lines).block(block), box_area);

            // Proxy form popup if active
            if let Some(ref form) = app.proxy_form {
                let fw: u16 = 50;
                let fh: u16 = if form.editing_idx.is_some() { 13 } else { 11 };
                let fx = (area.width.saturating_sub(fw)) / 2;
                let fy = (area.height.saturating_sub(fh)) / 2;
                let popup_area = Rect::new(fx, fy, fw, fh);
                f.render_widget(Clear, popup_area);

                let protocol = PROTOCOLS[form.protocol_idx];
                let mode = MODES[form.mode_idx];
                let remote_default_port = default_port(protocol);
                let mut rows: Vec<(usize, &str, &str, &str)> = vec![
                    (0, "name", &form.fields[0], ""),
                    (1, "protocol", protocol, ""),
                    (2, "mode", mode, ""),
                ];
                if form.mode_idx == 1 {
                    rows.push((3, "remote host", &form.fields[3], "127.0.0.1"));
                    rows.push((4, "remote port", &form.fields[4], remote_default_port));
                    rows.push((5, "interface", &form.fields[5], "lo0"));
                } else if form.editing_idx.is_some() {
                    rows.push((3, "listen host", &form.fields[1], "127.0.0.1"));
                    rows.push((4, "listen port", &form.fields[2], ""));
                    rows.push((5, "remote host", &form.fields[3], "127.0.0.1"));
                    rows.push((6, "remote port", &form.fields[4], remote_default_port));
                } else {
                    rows.push((3, "remote host", &form.fields[3], "127.0.0.1"));
                    rows.push((4, "remote port", &form.fields[4], remote_default_port));
                }
                let mut form_lines: Vec<Line> = Vec::new();
                for &(i, label, value, placeholder) in &rows {
                    let cursor = if i == form.active_field { "▌" } else { "" };
                    let label_style = if i == form.active_field { Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::DarkGray) };
                    let hint = if (i == 1 || i == 2) && i == form.active_field { " ◀ ▶" } else { "" };
                    let display = if value.is_empty() && !placeholder.is_empty() && i != form.active_field {
                        vec![Span::styled(format!("   {:>12}: ", label), label_style), Span::styled(placeholder.to_string(), Style::default().fg(Color::Rgb(80, 80, 80)))]
                    } else if value.is_empty() && !placeholder.is_empty() {
                        vec![Span::styled(format!("   {:>12}: ", label), label_style), Span::styled(cursor.to_string(), Style::default().fg(Color::White)), Span::styled(format!(" ({})", placeholder), Style::default().fg(Color::Rgb(80, 80, 80)))]
                    } else {
                        vec![Span::styled(format!("   {:>12}: ", label), label_style), Span::styled(format!("{}{}", value, cursor), Style::default().fg(Color::White)), Span::styled(hint.to_string(), Style::default().fg(Color::DarkGray))]
                    };
                    form_lines.push(Line::from(display));
                }
                form_lines.push(Line::from(""));
                if let Some(ref err) = form.error {
                    form_lines.push(Line::from(Span::styled(format!("   ⚠ {}", err), Style::default().fg(Color::Red))));
                }
                form_lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled("Esc", Style::default().fg(Color::DarkGray)), Span::raw(" cancel  "),
                    Span::styled("Enter", Style::default().fg(Color::Green)), Span::raw(" save"),
                ]));
                let popup = Paragraph::new(form_lines)
                    .block(Block::default().borders(Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded)
                        .border_style(Style::default().fg(Color::Cyan)).title(if form.editing_idx.is_some() { " Edit Proxy " } else { " Add Proxy " }));
                f.render_widget(popup, popup_area);
            }
        }
        AppMode::GroupDetail => {
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" Group: {}", app.dashboard.detail_group_name),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));

            if app.dashboard.detail_proxies.is_empty() {
                lines.push(Line::from(Span::styled(" No proxies", Style::default().fg(Color::DarkGray))));
            } else {
                for (i, p) in app.dashboard.detail_proxies.iter().enumerate() {
                    let is_selected = i == app.dashboard.detail_selected;
                    let prefix = if is_selected { " ▸ " } else { "   " };
                    let name_style = if is_selected {
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(prefix, Style::default().fg(Color::Cyan)),
                        Span::styled(format!("{:<10}", p.name), name_style),
                        Span::styled(format!(" {} \u{2192} {} ", p.listen, p.remote), Style::default().fg(Color::DarkGray)),
                    ]));
                }
            }
            lines.push(Line::from(""));
            if app.proxy_form.is_none() {
                lines.push(Line::from(Span::styled(
                    " n add  |  e edit  |  d delete  |  Esc back",
                    Style::default().fg(Color::DarkGray),
                )));
            }

            let box_h = lines.len() as u16 + 2;
            let x = (main_area.width.saturating_sub(box_w)) / 2;
            let y = (main_area.height.saturating_sub(box_h)) / 2;
            let box_area = Rect::new(x, y, box_w, box_h);
            let block = Block::default().borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .title_top(Line::from(vec![
                    Span::styled(" ", Style::default()),
                    Span::styled(app.dashboard.detail_group_name.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled(" ", Style::default()),
                ]));
            let content = Paragraph::new(lines).block(block);
            f.render_widget(content, box_area);

            // Proxy form popup
            if let Some(ref form) = app.proxy_form {
                let fw: u16 = 50;
                let fh: u16 = if form.editing_idx.is_some() { 13 } else { 11 };
                let fx = (area.width.saturating_sub(fw)) / 2;
                let fy = (area.height.saturating_sub(fh)) / 2;
                let popup_area = Rect::new(fx, fy, fw, fh);
                f.render_widget(Clear, popup_area);

                let protocol = PROTOCOLS[form.protocol_idx];
                let mode = MODES[form.mode_idx];
                let remote_default_port = default_port(protocol);
                let mut rows: Vec<(usize, &str, &str, &str)> = vec![
                    (0, "name", &form.fields[0], ""),
                    (1, "protocol", protocol, ""),
                    (2, "mode", mode, ""),
                ];
                if form.mode_idx == 1 {
                    rows.push((3, "remote host", &form.fields[3], "127.0.0.1"));
                    rows.push((4, "remote port", &form.fields[4], remote_default_port));
                    rows.push((5, "interface", &form.fields[5], "lo0"));
                } else if form.editing_idx.is_some() {
                    rows.push((3, "listen host", &form.fields[1], "127.0.0.1"));
                    rows.push((4, "listen port", &form.fields[2], ""));
                    rows.push((5, "remote host", &form.fields[3], "127.0.0.1"));
                    rows.push((6, "remote port", &form.fields[4], remote_default_port));
                } else {
                    rows.push((3, "remote host", &form.fields[3], "127.0.0.1"));
                    rows.push((4, "remote port", &form.fields[4], remote_default_port));
                }
                let mut form_lines: Vec<Line> = Vec::new();
                for &(i, label, value, placeholder) in &rows {
                    let cursor = if i == form.active_field { "▌" } else { "" };
                    let label_style = if i == form.active_field { Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::DarkGray) };
                    let hint = if (i == 1 || i == 2) && i == form.active_field { " ◀ ▶" } else { "" };
                    let display = if value.is_empty() && !placeholder.is_empty() && i != form.active_field {
                        vec![Span::styled(format!("   {:>12}: ", label), label_style), Span::styled(placeholder.to_string(), Style::default().fg(Color::Rgb(80, 80, 80)))]
                    } else if value.is_empty() && !placeholder.is_empty() {
                        vec![Span::styled(format!("   {:>12}: ", label), label_style), Span::styled(cursor.to_string(), Style::default().fg(Color::White)), Span::styled(format!(" ({})", placeholder), Style::default().fg(Color::Rgb(80, 80, 80)))]
                    } else {
                        vec![Span::styled(format!("   {:>12}: ", label), label_style), Span::styled(format!("{}{}", value, cursor), Style::default().fg(Color::White)), Span::styled(hint.to_string(), Style::default().fg(Color::DarkGray))]
                    };
                    form_lines.push(Line::from(display));
                }
                form_lines.push(Line::from(""));
                if let Some(ref err) = form.error {
                    form_lines.push(Line::from(Span::styled(format!("   ⚠ {}", err), Style::default().fg(Color::Red))));
                }
                form_lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled("Esc", Style::default().fg(Color::DarkGray)), Span::raw(" cancel  "),
                    Span::styled("Enter", Style::default().fg(Color::Green)), Span::raw(" save"),
                ]));
                let popup = Paragraph::new(form_lines)
                    .block(Block::default().borders(Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded)
                        .border_style(Style::default().fg(Color::Cyan)).title(if form.editing_idx.is_some() { " Edit Proxy " } else { " Add Proxy " }));
                f.render_widget(popup, popup_area);
            }

            // Delete confirm popup
            if app.dashboard.detail_delete_confirm {
                let w: u16 = 36;
                let h: u16 = 5;
                let x = (area.width.saturating_sub(w)) / 2;
                let y = (area.height.saturating_sub(h)) / 2;
                let popup_area = Rect::new(x, y, w, h);
                f.render_widget(Clear, popup_area);
                let name = app.dashboard.detail_proxies.get(app.dashboard.detail_selected).map(|p| p.name.as_str()).unwrap_or("");
                let lines = vec![
                    Line::from(format!(" Delete \"{}\"?", name)),
                    Line::from(""),
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled("y/Enter", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        Span::raw(" confirm  "),
                        Span::styled("n/Esc", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                        Span::raw(" cancel"),
                    ]),
                ];
                let popup = Paragraph::new(lines).block(Block::default().borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Yellow))
                    .title(" Confirm "));
                f.render_widget(popup, popup_area);
            }

            // Status bar
            let key_style = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
            let sep = Style::default().fg(Color::DarkGray);
            let status = Line::from(vec![
                Span::raw(" "),
                Span::styled("n", key_style), Span::raw(" add "),
                Span::styled("│", sep), Span::raw(" "),
                Span::styled("e", key_style), Span::raw(" edit "),
                Span::styled("│", sep), Span::raw(" "),
                Span::styled("d", key_style), Span::raw(" delete "),
                Span::styled("│", sep), Span::raw(" "),
                Span::styled("Esc", key_style), Span::raw(" back"),
            ]).centered();
            let status_block = Block::default().borders(Borders::TOP | Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray));
            let status_inner = status_block.inner(chunks[1]);
            f.render_widget(status_block, chunks[1]);
            f.render_widget(Paragraph::new(status), status_inner);
        }
        AppMode::Main => {}
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(f.area());

    let chunks = Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(0)])
        .split(outer[0]);

    // Left: component list
    let comp_focused = app.focus == Focus::Components || app.focus == Focus::ComponentFilter;
    let fuzzy = SkimMatcherV2::default();
    let filter_active = !app.component_filter.is_empty();

    // Build "ALL" row with group name
    let all_label: Line = if let Some(ref group) = app.active_group {
        Line::from(vec![
            Span::raw(if app.component_idx.is_none() { " > All " } else { "   All " }),
            Span::styled(format!("[{}]", group), Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(if app.component_idx.is_none() { " > All" } else { "   All" })
    };
    let all_style = if app.component_idx.is_none() && comp_focused {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    } else if app.component_idx.is_none() {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let mut items: Vec<ListItem> = vec![ListItem::new(all_label).style(all_style)];

    if filter_active {
        items.push(ListItem::new(Line::from(Span::styled(
            format!(" (filter: {})", app.component_filter),
            Style::default().fg(Color::Rgb(255, 165, 0)),
        ))));
        items.extend(app.components.iter().enumerate().filter(|(_, c)| {
            let target = format!("{} {} {}", c.name, c.listen, c.listen);
            fuzzy.fuzzy_match(&target, &app.component_filter).is_some()
        }).map(|(i, c)| {
            let selected = app.component_idx == Some(i);
            let prefix = if selected { " >" } else { "  " };
            let style = if selected && comp_focused {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let status = app.status_map.lock().unwrap().get(&c.name).cloned();
            let in_cooldown = status.as_ref()
                .and_then(|s| s.last_active_at.as_ref())
                .and_then(|t| t.elapsed().ok())
                .map(|d| d < Duration::from_secs(3))
                .unwrap_or(false);
            let (dot, dot_color) = match status {
                Some(s) if s.last_error.is_some() => ("◎", Color::Yellow),
                Some(s) if s.active_connections > 0 => ("●", Color::Green),
                _ if in_cooldown => ("●", Color::Green),
                Some(s) if s.has_connector => ("○", Color::DarkGray),
                _ => ("○", Color::Gray),
            };
            let count = app.events.iter().filter(|ev| ev.component == c.name).count();
            let count_style = if count > 0 { Style::default().fg(Color::Green) } else { Style::default().fg(Color::DarkGray) };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{} ", prefix)),
                Span::styled(dot, Style::default().fg(dot_color)),
                Span::styled(format!(" {}", c.name), style),
                Span::styled(format!(" {}", count), count_style),
            ])).style(style)
        }));
    } else {
        items.extend(app.components.iter().enumerate().map(|(i, c)| {
            let selected = app.component_idx == Some(i);
            let prefix = if selected { " >" } else { "  " };
            let style = if selected && comp_focused {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let status = app.status_map.lock().unwrap().get(&c.name).cloned();
            let in_cooldown = status.as_ref()
                .and_then(|s| s.last_active_at.as_ref())
                .and_then(|t| t.elapsed().ok())
                .map(|d| d < Duration::from_secs(3))
                .unwrap_or(false);
            let (dot, dot_color) = match status {
                Some(s) if s.last_error.is_some() => ("◎", Color::Yellow),
                Some(s) if s.active_connections > 0 => ("●", Color::Green),
                _ if in_cooldown => ("●", Color::Green),
                Some(s) if s.has_connector => ("○", Color::DarkGray),
                _ => ("○", Color::Gray),
            };
            let count = app.events.iter().filter(|ev| ev.component == c.name).count();
            let count_style = if count > 0 { Style::default().fg(Color::Green) } else { Style::default().fg(Color::DarkGray) };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{} ", prefix)),
                Span::styled(dot, Style::default().fg(dot_color)),
                Span::styled(format!(" {}", c.name), style),
                Span::styled(format!(" {}", count), count_style),
            ])).style(style)
        }));
    };

    let comp_title = format!(" Ocular v{} ", env!("CARGO_PKG_VERSION"));
    let comp_border = if comp_focused { Style::default().fg(Color::Cyan) } else { Style::default().fg(Color::DarkGray) };

    // Split component area: list + optional filter input at bottom
    if app.focus == Focus::ComponentFilter {
        let comp_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(chunks[0]);
        let left = List::new(items)
            .block(Block::default().borders(Borders::TOP).border_style(comp_border).title(comp_title));
        f.render_widget(left, comp_chunks[0]);
        let filter_line = Paragraph::new(Line::from(Span::styled(
            format!(" /{}▌", app.component_filter),
            Style::default().fg(Color::Rgb(255, 165, 0)),
        )));
        f.render_widget(filter_line, comp_chunks[1]);
    } else {
        let left = List::new(items)
            .block(Block::default().borders(Borders::TOP).border_style(comp_border).title(comp_title));
        f.render_widget(left, chunks[0]);
    }

    // Right: vertical split
    let right = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1]);

    // Event stream
    let filtered = app.filtered_events();
    let events_focused = app.focus == Focus::Events;
    let visible_height = right[0].height.saturating_sub(2) as usize;
    let scroll_margin: usize = 3;
    let visible_start = if app.selected + scroll_margin < visible_height {
        0
    } else {
        app.selected + scroll_margin - visible_height + 1
    };
    let theme = &app.theme;
    let event_items: Vec<ListItem> = filtered.iter().enumerate()
        .skip(visible_start)
        .take(visible_height)
        .map(|(idx, (orig_idx, ev, match_indices))| {
            let time = format_time(&ev.timestamp);
            let lat = format_latency(&ev.latency);
            let spans: Vec<Span> = app.event_format.segments.iter().flat_map(|seg| {
                match seg {
                    FormatSegment::Literal(s) => vec![Span::raw(s.clone())],
                    FormatSegment::Field { name, width } => {
                        let (raw, style) = match name.as_str() {
                            "index" => (format!("{}", orig_idx + 1), theme.line_number),
                            "time" => (time.clone(), theme.timestamp),
                            "component" => (ev.component.to_string(), if ev.system { Style::default().fg(Color::Red) } else { theme.component_style(ev.protocol) }),
                            "command" => (ev.command.clone(), if ev.system { Style::default().fg(Color::Red) } else { theme.command }),
                            "latency" => {
                                let style = if app.latency_threshold_ms.is_some_and(|t| ev.latency.as_secs_f64() * 1000.0 > t) {
                                    Style::default().fg(Color::Red)
                                } else {
                                    theme.latency
                                };
                                (lat.to_string(), style)
                            },
                            "process" => (ev.process.clone().unwrap_or_default(), theme.latency),
                            "src" => (ev.src.clone().unwrap_or_default(), Style::default().fg(Color::Blue)),
                            "dest" => (ev.dest.clone().unwrap_or_default(), Style::default().fg(Color::Cyan)),
                            _ => (String::new(), Style::default()),
                        };
                        let formatted = match width {
                            Some(w) if *w > 0 => format!("{:>width$}", raw, width = *w as usize),
                            Some(w) if *w < 0 => format!("{:<width$}", raw, width = (-*w) as usize),
                            _ => raw,
                        };
                        // Highlight matched chars in command field
                        if name == "command" && !match_indices.is_empty() {
                            let highlight = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
                            let chars: Vec<char> = formatted.chars().collect();
                            let mut result: Vec<Span> = Vec::new();
                            let mut i = 0;
                            while i < chars.len() {
                                if match_indices.contains(&i) {
                                    let start = i;
                                    while i < chars.len() && match_indices.contains(&i) { i += 1; }
                                    result.push(Span::styled(chars[start..i].iter().collect::<String>(), highlight));
                                } else {
                                    let start = i;
                                    while i < chars.len() && !match_indices.contains(&i) { i += 1; }
                                    result.push(Span::styled(chars[start..i].iter().collect::<String>(), style));
                                }
                            }
                            return result;
                        }
                        vec![Span::styled(formatted, style)]
                    }
                }
            }).collect();
            let line = Line::from(spans);
            let in_visual = app.visual_mode && {
                let lo = app.visual_anchor.min(app.selected);
                let hi = app.visual_anchor.max(app.selected);
                idx >= lo && idx <= hi
            };
            let style = if idx == app.selected {
                theme.selected
            } else if in_visual {
                theme.visual
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        }).collect();
    let filter_info = if app.filter.is_empty() { String::new() } else { format!(" [filter: {}]", app.filter) };
    let count_info = if !app.filter.is_empty() || app.component_idx.is_some() {
        format!(" ({}/{})", filtered.len(), app.events.len())
    } else {
        String::new()
    };
    let events_border = if events_focused { Style::default().fg(Color::Cyan) } else { Style::default().fg(Color::DarkGray) };
    let paused_info = if app.paused { " ⏸ PAUSED" } else { "" };
    let ek = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
    let mut title_spans = vec![
        Span::raw(" Events ("),
        Span::styled("y", ek), Span::raw(": copy, "),
        Span::styled("Enter", ek), Span::raw(": detail, "),
        Span::styled("e", ek), Span::raw(": edit, "),
        Span::styled("v", ek), Span::raw(": visual)"),
    ];
    if app.paused {
        title_spans.push(Span::styled(paused_info, Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)));
    }
    if !filter_info.is_empty() {
        title_spans.push(Span::styled(filter_info, Style::default().fg(Color::Rgb(255, 165, 0))));
    }
    if !count_info.is_empty() {
        title_spans.push(Span::raw(format!("{} ", count_info)));
    }
    let event_list = List::new(event_items)
        .block(Block::default().borders(Borders::TOP).border_style(events_border)
            .title(Line::from(title_spans)));
    f.render_widget(event_list, right[0]);

    // Detail panel
    let detail_focused = app.focus == Focus::Detail;
    let selected_event = filtered.get(app.selected).map(|(_, ev, _)| *ev);
    let (detail_text, detail_meta): (Text, Line) = if let Some(ev) = selected_event {
        let mut lines: Vec<Line> = Vec::new();

        if ev.protocol == ocular_protocol::Protocol::Amqp {
            // AMQP: distinguish Publish (send) vs Deliver (receive) vs request-response
            let is_publish = ev.command.contains("Basic.Publish");
            let is_deliver = ev.command.contains("Basic.Deliver");
            if is_publish {
                // Extract body from full_command (after "Body: ")
                let (via, body) = ev.full_command.split_once("\nBody: ")
                    .map(|(v, b)| (v.to_string(), b.to_string()))
                    .unwrap_or_else(|| (ev.full_command.clone(), String::new()));
                if !body.is_empty() {
                    lines.push(Line::from(Span::styled(format!("Send: {}", body), Style::default().fg(Color::Cyan))));
                }
                lines.push(Line::from(format!("Via:  {}", via)));
            } else if is_deliver {
                let body = if ev.response.is_empty() { ev.response_detail.clone() } else { ev.response.clone() };
                if !body.is_empty() {
                    lines.push(Line::from(Span::styled(format!("Received: {}", body), Style::default().fg(Color::Green))));
                }
                lines.push(Line::from(format!("Via:      {}", ev.full_command)));
            } else {
                // Normal request-response (e.g. Basic.Get, Queue.Declare)
                lines.push(Line::from(Span::styled(ev.full_command.clone(), Style::default().fg(Color::Cyan))));
                if !ev.response_detail.is_empty() {
                    lines.push(Line::from(""));
                    for rd in ev.response_detail.lines() {
                        lines.push(Line::from(rd.to_string()));
                    }
                }
            }
        } else {
            // MySQL / Postgres / Redis: request, response
            let formatted_cmd = if ev.protocol == ocular_protocol::Protocol::Mysql {
                format_sql(&ev.full_command)
            } else {
                ev.full_command.clone()
            };
            for sql_line in formatted_cmd.lines() {
                lines.push(highlight_sql_line(sql_line));
            }
            // Response detail
            if !ev.response_detail.is_empty() {
                lines.push(Line::from(""));
                let mut in_json = false;
                for rd in ev.response_detail.lines() {
                    if rd == "[Response Body]" || rd == "[Request Body]" {
                        in_json = ev.protocol == ocular_protocol::Protocol::Http;
                        lines.push(Line::from(Span::styled(rd.to_string(), Style::default().fg(Color::DarkGray))));
                    } else if rd.starts_with('[') && rd.ends_with(']') {
                        in_json = false;
                        lines.push(Line::from(Span::styled(rd.to_string(), Style::default().fg(Color::DarkGray))));
                    } else if in_json {
                        lines.push(highlight_json_line(rd));
                    } else {
                        lines.push(Line::from(rd.to_string()));
                    }
                }
            }
        }

        // Build metadata line
        let mut meta_parts: Vec<Span> = Vec::new();
        meta_parts.push(Span::raw(format!("{}  ", format_time(&ev.timestamp))));
        if let Some(s) = &ev.src {
            meta_parts.push(Span::styled(format!("{}  ", s), Style::default().fg(Color::Blue)));
        }
        if let Some(d) = &ev.dest {
            meta_parts.push(Span::styled(format!("→ {}  ", d), Style::default().fg(Color::Cyan)));
        }
        if let Some(p) = &ev.process {
            meta_parts.push(Span::styled(format!("{}  ", p), Style::default().fg(Color::DarkGray)));
        }
        if ev.latency.as_nanos() > 0 {
            meta_parts.push(Span::styled(format_latency(&ev.latency), Style::default().fg(Color::Yellow)));
        }
        let meta_line = Line::from(meta_parts);

        (Text::from(lines), meta_line)
    } else {
        (Text::from("No events yet. Waiting for traffic..."), Line::from(""))
    };
    let detail_str_for_scroll: String = detail_text.lines.iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>().join("\n");
    let detail_border = if detail_focused { Style::default().fg(Color::Cyan) } else { Style::default().fg(Color::DarkGray) };
    let key_hint = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
    let title = if detail_focused {
        Line::from(vec![
            Span::raw(" Detail ("),
            Span::styled("j/k", key_hint), Span::raw(": scroll, "),
            Span::styled("e", key_hint), Span::raw(": edit, "),
            Span::styled("esc", key_hint), Span::raw(": back to Events) "),
        ])
    } else {
        Line::from(" Detail ")
    };
    // Clamp scroll
    let detail_view_width = right[1].width.saturating_sub(2).max(1) as usize;
    let wrapped_lines: u16 = detail_str_for_scroll.lines()
        .map(|l| l.chars().count().max(1).div_ceil(detail_view_width) as u16)
        .sum();
    let max_scroll = wrapped_lines.saturating_sub(1);
    app.detail_scroll = app.detail_scroll.min(max_scroll);

    // Split detail area: main content + 1-line sticky footer
    let detail_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(right[1]);

    let detail_widget = Paragraph::new(detail_text)
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0))
        .block(Block::default().borders(Borders::TOP).border_style(detail_border).title(title)
            .padding(ratatui::widgets::Padding::left(1)));
    f.render_widget(detail_widget, detail_chunks[0]);

    let meta_widget = Paragraph::new(detail_meta)
        .block(Block::default().padding(ratatui::widgets::Padding::horizontal(2)));
    f.render_widget(meta_widget, detail_chunks[1]);

    // Bottom status bar
    let key_style = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
    let sep = Style::default().fg(Color::DarkGray);
    let status_line: (Line, Line) = if app.focus == Focus::Filter {
        (Line::from(Span::styled(format!("/{}", app.filter), Style::default().fg(Color::Yellow))), Line::from(""))
    } else {
        let mode_span = if app.leader_active {
            Span::styled(" LEADER ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        } else if app.visual_mode {
            Span::styled(" VISUAL ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(" NORMAL ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
        };
        let follow_span = if app.follow {
            Span::styled(" FOLLOW ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        } else {
            Span::raw("")
        };
        let left_line = Line::from(vec![
            Span::raw(" "),
            Span::styled("Tab", key_style), Span::raw(" cycle "),
            Span::styled("│", sep), Span::raw(" "),
            Span::styled("/", key_style), Span::raw(" filter "),
            Span::styled("│", sep), Span::raw(" "),
            Span::styled("j/k", key_style), Span::raw(" navigate "),
            Span::styled("│", sep), Span::raw(" "),
            Span::styled("Space", key_style), Span::raw(" menu "),
            Span::styled("│", sep), Span::raw(" "),
            Span::styled("q", key_style), Span::raw(" quit"),
        ]);
        let right_line = Line::from(vec![follow_span, mode_span]);
        (left_line, right_line)
    };
    let status_block = Block::default().borders(Borders::TOP | Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray));
    let status_inner = status_block.inner(outer[1]);
    f.render_widget(status_block, outer[1]);
    let status_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(8)])
        .split(status_inner);
    f.render_widget(Paragraph::new(status_line.0), status_chunks[0]);
    f.render_widget(Paragraph::new(status_line.1).alignment(ratatui::layout::Alignment::Right), status_chunks[1]);

    // Leader menu
    if app.leader_active && app.show_leader_menu {
        let menu_lines = vec![
            Line::from(Span::styled(" Space Leader Menu", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from(vec![Span::styled(" h", Style::default().fg(Color::Cyan)), Span::raw("  → Left panel")]),
            Line::from(vec![Span::styled(" j", Style::default().fg(Color::Cyan)), Span::raw("  → Below panel")]),
            Line::from(vec![Span::styled(" k", Style::default().fg(Color::Cyan)), Span::raw("  → Above panel")]),
            Line::from(vec![Span::styled(" l", Style::default().fg(Color::Cyan)), Span::raw("  → Right panel")]),
            Line::from(vec![Span::styled(" c", Style::default().fg(Color::Cyan)), Span::raw("  → Clear all events")]),
            Line::from(vec![Span::styled(" f", Style::default().fg(Color::Cyan)), Span::raw("  → Toggle follow (tail -f)")]),
            Line::from(vec![Span::styled(" p", Style::default().fg(Color::Cyan)), Span::raw("  → Pause/resume stream")]),
            Line::from(vec![Span::styled(" ,", Style::default().fg(Color::Cyan)), Span::raw("  → Edit config")]),
            Line::from(vec![Span::styled(" g", Style::default().fg(Color::Cyan)), Span::raw("  → Switch group")]),
            Line::from(""),
            Line::from(Span::styled(" Esc/any  → cancel", Style::default().fg(Color::DarkGray))),
        ];
        let menu_height = menu_lines.len() as u16 + 2;
        let menu_width = 28;
        let area = f.area();
        let x = area.width.saturating_sub(menu_width + 1);
        let y = area.height.saturating_sub(menu_height + 1);
        let popup_area = Rect::new(x, y, menu_width, menu_height);
        f.render_widget(Clear, popup_area);
        let popup = Paragraph::new(menu_lines)
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Yellow)));
        f.render_widget(popup, popup_area);
    }

    // Confirm quit
    if app.confirm_quit {
        let msg = Line::from(vec![
            Span::raw(" Quit? "),
            Span::styled("y", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw("/"),
            Span::styled("n", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::raw(" "),
        ]);
        let w: u16 = 12;
        let area = f.area();
        let x = (area.width.saturating_sub(w)) / 2;
        let y = (area.height.saturating_sub(3)) / 2;
        let popup_area = Rect::new(x, y, w, 3);
        f.render_widget(Clear, popup_area);
        f.render_widget(
            Paragraph::new(msg).alignment(ratatui::layout::Alignment::Center)
                .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Yellow))),
            popup_area,
        );
    }

    // Help popup
    if app.help_active {
        let help_lines = vec![
            Line::from(Span::styled(" Keybindings", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from(vec![Span::styled(" Navigation", Style::default().add_modifier(Modifier::BOLD))]),
            Line::from(vec![Span::styled("  j/k       ", Style::default().fg(Color::Green)), Span::raw("Navigate / scroll")]),
            Line::from(vec![Span::styled("  h/l       ", Style::default().fg(Color::Green)), Span::raw("Switch panel left/right")]),
            Line::from(vec![Span::styled("  Tab       ", Style::default().fg(Color::Green)), Span::raw("Next panel")]),
            Line::from(vec![Span::styled("  gg        ", Style::default().fg(Color::Green)), Span::raw("Jump to first")]),
            Line::from(vec![Span::styled("  G         ", Style::default().fg(Color::Green)), Span::raw("Jump to last")]),
            Line::from(vec![Span::styled("  Ngg       ", Style::default().fg(Color::Green)), Span::raw("Jump to line N")]),
            Line::from(""),
            Line::from(vec![Span::styled(" Actions", Style::default().add_modifier(Modifier::BOLD))]),
            Line::from(vec![Span::styled("  /         ", Style::default().fg(Color::Green)), Span::raw("Filter events")]),
            Line::from(vec![Span::styled("  Enter     ", Style::default().fg(Color::Green)), Span::raw("Select component")]),
            Line::from(vec![Span::styled("  v         ", Style::default().fg(Color::Green)), Span::raw("Visual selection")]),
            Line::from(vec![Span::styled("  y         ", Style::default().fg(Color::Green)), Span::raw("Yank to clipboard")]),
            Line::from(vec![Span::styled("  e         ", Style::default().fg(Color::Green)), Span::raw("Open in $EDITOR")]),
            Line::from(vec![Span::styled("  Esc       ", Style::default().fg(Color::Green)), Span::raw("Back / clear filter")]),
            Line::from(""),
            Line::from(vec![Span::styled(" Leader (Space)", Style::default().add_modifier(Modifier::BOLD))]),
            Line::from(vec![Span::styled("  c         ", Style::default().fg(Color::Green)), Span::raw("Clear all events")]),
            Line::from(vec![Span::styled("  f         ", Style::default().fg(Color::Green)), Span::raw("Toggle follow")]),
            Line::from(vec![Span::styled("  p         ", Style::default().fg(Color::Green)), Span::raw("Pause/resume")]),
            Line::from(vec![Span::styled("  ,         ", Style::default().fg(Color::Green)), Span::raw("Edit config")]),
            Line::from(""),
            Line::from(Span::styled(" ?  toggle this help    q  quit", Style::default().fg(Color::DarkGray))),
        ];
        let help_height = help_lines.len() as u16 + 2;
        let help_width = 40;
        let area = f.area();
        let x = (area.width.saturating_sub(help_width)) / 2;
        let y = (area.height.saturating_sub(help_height)) / 2;
        let help_area = Rect::new(x, y, help_width, help_height);
        f.render_widget(Clear, help_area);
        let help_popup = Paragraph::new(help_lines)
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Cyan)));
        f.render_widget(help_popup, help_area);
    }

    // Proxy form popup
    if let Some(ref form) = app.proxy_form {
        let area = f.area();
        let w: u16 = 54;
        let h: u16 = if form.mode_idx == 1 { 15 } else if form.editing_idx.is_some() { 16 } else { 14 };
        let x = (area.width.saturating_sub(w)) / 2;
        let y = (area.height.saturating_sub(h)) / 2;
        let popup_area = Rect::new(x, y, w, h);
        f.render_widget(Clear, popup_area);

        let title = if form.editing_idx.is_some() { " Edit Proxy " } else { " New Proxy " };
        let protocol = PROTOCOLS[form.protocol_idx];
        let mode = MODES[form.mode_idx];
        let remote_default_port = default_port(protocol);

        // field_idx, label, value, placeholder
        let mut rows: Vec<(usize, &str, &str, &str)> = vec![
            (0, "name", &form.fields[0], ""),
            (1, "protocol", protocol, ""),
            (2, "mode", mode, ""),
        ];
        if form.mode_idx == 1 {
            rows.push((3, "remote host", &form.fields[3], "127.0.0.1"));
            rows.push((4, "remote port", &form.fields[4], remote_default_port));
            rows.push((5, "interface", &form.fields[5], "lo0"));
        } else if form.editing_idx.is_some() {
            rows.push((3, "listen host", &form.fields[1], "127.0.0.1"));
            rows.push((4, "listen port", &form.fields[2], ""));
            rows.push((5, "remote host", &form.fields[3], "127.0.0.1"));
            rows.push((6, "remote port", &form.fields[4], remote_default_port));
        } else {
            rows.push((3, "remote host", &form.fields[3], "127.0.0.1"));
            rows.push((4, "remote port", &form.fields[4], remote_default_port));
        }

        let mut lines: Vec<Line> = Vec::new();
        for &(i, label, value, placeholder) in &rows {
            let cursor = if i == form.active_field { "▌" } else { "" };
            let label_style = if i == form.active_field {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let hint = if (i == 1 || i == 2) && i == form.active_field { " ◀ ▶" } else { "" };
            let display = if value.is_empty() && !placeholder.is_empty() && i != form.active_field {
                vec![
                    Span::styled(format!("   {:>12}: ", label), label_style),
                    Span::styled(placeholder.to_string(), Style::default().fg(Color::Rgb(80, 80, 80))),
                ]
            } else if value.is_empty() && !placeholder.is_empty() {
                vec![
                    Span::styled(format!("   {:>12}: ", label), label_style),
                    Span::styled(cursor.to_string(), Style::default().fg(Color::White)),
                    Span::styled(format!(" ({})", placeholder), Style::default().fg(Color::Rgb(80, 80, 80))),
                ]
            } else {
                vec![
                    Span::styled(format!("   {:>12}: ", label), label_style),
                    Span::styled(format!("{}{}", value, cursor), Style::default().fg(Color::White)),
                    Span::styled(hint.to_string(), Style::default().fg(Color::DarkGray)),
                ]
            };
            lines.push(Line::from(display));
        }
        lines.push(Line::from(""));
        if let Some(ref err) = form.error {
            lines.push(Line::from(Span::styled(format!("   ⚠ {}", err), Style::default().fg(Color::Red))));
        }
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled("Esc", Style::default().fg(Color::DarkGray)),
            Span::raw(" cancel  "),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" submit"),
        ]));

        let popup = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title));
        f.render_widget(popup, popup_area);
    }

    // Delete confirm popup
    if let Some(idx) = app.delete_confirm_idx {
        let name = app.components.get(idx).map(|c| c.name.as_str()).unwrap_or("?");
        let area = f.area();
        let w: u16 = 36;
        let h: u16 = 5;
        let x = (area.width.saturating_sub(w)) / 2;
        let y = (area.height.saturating_sub(h)) / 2;
        let popup_area = Rect::new(x, y, w, h);
        f.render_widget(Clear, popup_area);
        let lines = vec![
            Line::from(format!(" Delete \"{}\"?", name)),
            Line::from(""),
            Line::from(vec![
                Span::raw(" "),
                Span::styled("y/Enter", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::raw(" confirm  "),
                Span::styled("n/Esc", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::raw(" cancel"),
            ]),
        ];
        let popup = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" Confirm Delete "));
        f.render_widget(popup, popup_area);
    }

    // Info popup
    if let Some(idx) = app.info_popup_idx {
        if let Some(ci) = app.components.get(idx) {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(" name:   ", Style::default().fg(Color::DarkGray)),
                    Span::styled(ci.name.clone(), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled(" listen: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(ci.listen.clone()),
                ]),
            ];
            // Load protocol and remote from config
            if let Ok(content) = std::fs::read_to_string(&app.config_path) {
                if let Ok(cfg) = toml::from_str::<ReloadableConfig>(&content) {
                    if let Some(p) = cfg.proxy.iter().find(|p| p.name == ci.name) {
                        lines.insert(1, Line::from(vec![
                            Span::styled(" proto:  ", Style::default().fg(Color::DarkGray)),
                            Span::raw(p.protocol.clone()),
                        ]));
                        lines.push(Line::from(vec![
                            Span::styled(" remote: ", Style::default().fg(Color::DarkGray)),
                            Span::raw(p.remote.clone()),
                        ]));
                    }
                }
            }
            let stats = app.component_stats.get(&ci.name);
            let count = stats.map_or(0, |s| s.count);
            lines.push(Line::from(vec![
                Span::styled(" events: ", Style::default().fg(Color::DarkGray)),
                Span::styled(count.to_string(), Style::default().fg(Color::Yellow)),
            ]));
            if let Some(s) = stats {
                lines.push(Line::from(vec![
                    Span::styled(" qps:    ", Style::default().fg(Color::DarkGray)),
                    Span::raw(format!("{:.1}", s.qps())),
                ]));
                if !s.latencies.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled(" latency: ", Style::default().fg(Color::DarkGray)),
                        Span::raw(format!("min={:.2}ms avg={:.2}ms max={:.2}ms p95={:.2}ms",
                            s.latency_min.as_secs_f64() * 1000.0,
                            s.avg_latency().as_secs_f64() * 1000.0,
                            s.latency_max.as_secs_f64() * 1000.0,
                            s.p95_latency().as_secs_f64() * 1000.0,
                        )),
                    ]));
                }
                if s.error_count > 0 {
                    lines.push(Line::from(vec![
                        Span::styled(" errors: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("{} ({:.1}%)", s.error_count, s.error_rate()), Style::default().fg(Color::Red)),
                    ]));
                }
            }

            let area = f.area();
            let w: u16 = 60;
            let h = lines.len() as u16 + 2;
            let x = (area.width.saturating_sub(w)) / 2;
            let y = (area.height.saturating_sub(h)) / 2;
            let popup_area = Rect::new(x, y, w, h);
            f.render_widget(Clear, popup_area);
            let popup = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(" Proxy Info (i to close) "));
            f.render_widget(popup, popup_area);
        }
    }

    // Group picker popup
    if let Some(ref picker) = app.group_picker {
        let area = f.area();
        let h = (picker.groups.len() as u16 + 2).min(area.height - 4);
        let w: u16 = 30;
        let x = (area.width.saturating_sub(w)) / 2;
        let y = (area.height.saturating_sub(h)) / 2;
        let popup_area = Rect::new(x, y, w, h);
        f.render_widget(Clear, popup_area);
        let items: Vec<ListItem> = picker.groups.iter().enumerate().map(|(i, g)| {
            let is_active = app.active_group.as_deref() == Some(g.as_str());
            let prefix = if i == picker.selected { " ●" } else { "  " };
            let suffix = if is_active { " ✓" } else { "" };
            let style = if i == picker.selected {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else if is_active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(format!("{} {}{}", prefix, g, suffix)).style(style)
        }).collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(255, 165, 0)))
                .title(" Switch Group (j/k, Enter) "));
        f.render_widget(list, popup_area);
    }
}

fn copy_to_clipboard(text: &str) {
    use std::process::{Command, Stdio};
    use std::io::Write;
    let mut child = if cfg!(target_os = "macos") {
        Command::new("pbcopy")
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
    } else {
        Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null())
            .spawn()
            .or_else(|_| Command::new("wl-copy")
                .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null())
                .spawn())
    };
    if let Ok(ref mut child) = child {
        if let Some(ref mut stdin) = child.stdin {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

fn get_selected_commands(filtered: &[(usize, &ProxyEvent, Vec<usize>)], app: &App) -> String {
    if app.visual_mode {
        let lo = app.visual_anchor.min(app.selected);
        let hi = app.visual_anchor.max(app.selected);
        filtered.iter()
            .enumerate()
            .filter(|(idx, _)| *idx >= lo && *idx <= hi)
            .map(|(_, (_, ev, _))| format_copy_text(ev))
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        filtered.get(app.selected)
            .map(|(_, ev, _)| format_copy_text(ev))
            .unwrap_or_default()
    }
}

fn format_copy_text(ev: &ProxyEvent) -> String {
    ocular_protocol::get_handler(ev.protocol).to_replay_command(ev)
}

fn open_in_editor(text: &str) {
    use std::io::Write;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".to_string());
    let mut tmp = std::env::temp_dir();
    tmp.push("ocular_edit.sql");
    if let Ok(mut f) = std::fs::File::create(&tmp) {
        let _ = f.write_all(text.as_bytes());
    }
    let _ = std::process::Command::new(&editor)
        .arg(&tmp)
        .status();
}

fn highlight_sql_line(line: &str) -> Line<'static> {
    let keywords: &[&str] = &[
        "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IN", "ON", "AS",
        "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "CROSS", "FULL",
        "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE",
        "CREATE", "ALTER", "DROP", "TABLE", "INDEX", "VIEW",
        "ORDER", "BY", "GROUP", "HAVING", "LIMIT", "OFFSET",
        "UNION", "ALL", "DISTINCT", "EXISTS", "BETWEEN", "LIKE",
        "IS", "NULL", "TRUE", "FALSE", "CASE", "WHEN", "THEN", "ELSE", "END",
        "ASC", "DESC", "COUNT", "SUM", "AVG", "MIN", "MAX",
    ];
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut remaining = line;

    while !remaining.is_empty() {
        // Skip leading whitespace
        let ws_len = remaining.len() - remaining.trim_start().len();
        if ws_len > 0 {
            spans.push(Span::raw(remaining[..ws_len].to_string()));
            remaining = &remaining[ws_len..];
            continue;
        }
        // Try to match a word
        let word_len = remaining.chars().take_while(|c| c.is_alphanumeric() || *c == '_').count();
        if word_len > 0 {
            let word = &remaining[..word_len];
            if keywords.contains(&word.to_uppercase().as_str()) && word.chars().all(|c| c.is_uppercase() || c == '_') {
                spans.push(Span::styled(word.to_string(), Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)));
            } else if word.chars().all(|c| c.is_ascii_digit()) {
                spans.push(Span::styled(word.to_string(), Style::default().fg(Color::Yellow)));
            } else {
                spans.push(Span::styled(word.to_string(), Style::default().fg(Color::White)));
            }
            remaining = &remaining[word_len..];
        } else {
            // Single character (operator, punctuation, quote)
            let ch = remaining.chars().next().unwrap();
            let ch_len = ch.len_utf8();
            let style = match ch {
                '\'' | '"' => {
                    // String literal: consume until matching quote
                    let end = remaining[ch_len..].find(ch).map(|i| i + ch_len + ch_len).unwrap_or(remaining.len());
                    let s = remaining[..end].to_string();
                    remaining = &remaining[end..];
                    spans.push(Span::styled(s, Style::default().fg(Color::Green)));
                    continue;
                }
                '(' | ')' | ',' | ';' => Style::default().fg(Color::DarkGray),
                '=' | '<' | '>' | '!' | '+' | '-' | '*' => Style::default().fg(Color::Cyan),
                _ => Style::default().fg(Color::White),
            };
            spans.push(Span::styled(remaining[..ch_len].to_string(), style));
            remaining = &remaining[ch_len..];
        }
    }
    Line::from(spans)
}

fn highlight_json_line(line: &str) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut remaining = line;
    while !remaining.is_empty() {
        // Whitespace
        let ws = remaining.len() - remaining.trim_start().len();
        if ws > 0 {
            spans.push(Span::raw(remaining[..ws].to_string()));
            remaining = &remaining[ws..];
            continue;
        }
        let ch = remaining.chars().next().unwrap();
        match ch {
            '"' => {
                // String: find closing quote
                let end = remaining[1..].find('"').map(|i| i + 2).unwrap_or(remaining.len());
                let s = &remaining[..end];
                // Check if it's a key (followed by ':')
                let after = remaining[end..].trim_start();
                let style = if after.starts_with(':') {
                    Style::default().fg(Color::Cyan) // key
                } else {
                    Style::default().fg(Color::Green) // string value
                };
                spans.push(Span::styled(s.to_string(), style));
                remaining = &remaining[end..];
            }
            ':' => {
                spans.push(Span::styled(":".to_string(), Style::default().fg(Color::DarkGray)));
                remaining = &remaining[1..];
            }
            '{' | '}' | '[' | ']' => {
                spans.push(Span::styled(ch.to_string(), Style::default().fg(Color::Yellow)));
                remaining = &remaining[ch.len_utf8()..];
            }
            't' | 'f' | 'n' => {
                // true/false/null
                let word_len = remaining.chars().take_while(|c| c.is_alphabetic()).count();
                let word = &remaining[..word_len];
                if word == "true" || word == "false" || word == "null" {
                    spans.push(Span::styled(word.to_string(), Style::default().fg(Color::Magenta)));
                } else {
                    spans.push(Span::raw(word.to_string()));
                }
                remaining = &remaining[word_len..];
            }
            '0'..='9' | '-' => {
                // Number
                let num_len = remaining.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-' || *c == 'e' || *c == 'E' || *c == '+').count();
                spans.push(Span::styled(remaining[..num_len].to_string(), Style::default().fg(Color::Yellow)));
                remaining = &remaining[num_len..];
            }
            _ => {
                spans.push(Span::raw(ch.to_string()));
                remaining = &remaining[ch.len_utf8()..];
            }
        }
    }
    Line::from(spans)
}


#[allow(clippy::too_many_arguments)]
fn save_proxy_config(config_path: &std::path::Path, _components: &[ComponentInfo], protocol: &str, editing_idx: Option<usize>, name: &str, listen: &str, remote: &str, mode: &str, interface: &str) {
    let Ok(content) = std::fs::read_to_string(config_path) else { return };
    let Ok(mut doc) = content.parse::<toml_edit::DocumentMut>() else { return };

    let proxies = doc.entry("proxy").or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    let toml_edit::Item::ArrayOfTables(arr) = proxies else { return };

    if let Some(idx) = editing_idx {
        // Edit existing
        if let Some(table) = arr.get_mut(idx) {
            table["name"] = toml_edit::value(name);
            table["protocol"] = toml_edit::value(protocol);
            table["remote"] = toml_edit::value(remote);
            if mode == "capture" {
                table["mode"] = toml_edit::value("capture");
                table["interface"] = toml_edit::value(interface);
                table.remove("listen");
            } else {
                table["listen"] = toml_edit::value(listen);
                table.remove("mode");
                table.remove("interface");
            }
        }
    } else {
        // Add new
        let mut table = toml_edit::Table::new();
        table["name"] = toml_edit::value(name);
        table["protocol"] = toml_edit::value(protocol);
        table["remote"] = toml_edit::value(remote);
        if mode == "capture" {
            table["mode"] = toml_edit::value("capture");
            table["interface"] = toml_edit::value(interface);
        } else {
            table["listen"] = toml_edit::value(listen);
        }
        arr.push(table);
    }

    let _ = std::fs::write(config_path, doc.to_string());
}

fn format_proxy_toml(name: &str, protocol: &str, listen: &str, remote: &str, mode: &str, interface: &str) -> String {
    let mut s = format!("[[proxy]]\nname = \"{}\"\nprotocol = \"{}\"\nremote = \"{}\"\n", name, protocol, remote);
    if mode == "capture" {
        s.push_str(&format!("mode = \"capture\"\ninterface = \"{}\"\n", interface));
    } else if !listen.is_empty() {
        s.push_str(&format!("listen = \"{}\"\n", listen));
    }
    s.push('\n');
    s
}

fn delete_proxy_from_config(config_path: &std::path::Path, name: &str) {
    let Ok(content) = std::fs::read_to_string(config_path) else { return };
    let Ok(mut doc) = content.parse::<toml_edit::DocumentMut>() else { return };

    let Some(toml_edit::Item::ArrayOfTables(arr)) = doc.get_mut("proxy") else { return };
    let idx = arr.iter().position(|t| t.get("name").and_then(|v| v.as_str()) == Some(name));
    if let Some(idx) = idx {
        arr.remove(idx);
    }

    let _ = std::fs::write(config_path, doc.to_string());
}
