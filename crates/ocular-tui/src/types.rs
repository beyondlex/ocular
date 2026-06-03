use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ocular_proxy::{ProxyEvent, StatusMap};
use serde::Deserialize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast;

use crate::theme::ThemeConfig;

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
pub(crate) struct ExcludeMatcher {
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

    pub(crate) fn new(excludes: Option<&Vec<ExcludeConfig>>, include: Option<&ExcludeConfig>) -> Self {
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

    pub(crate) fn is_noop(&self) -> bool {
        self.excludes.is_empty() && self.includes.is_empty()
    }

    pub(crate) fn is_excluded(&self, text: &str) -> bool {
        if self.excludes.is_empty() { return false; }
        // include overrides exclude
        if !self.includes.is_empty() && Self::matches_any(&self.includes, text) {
            return false;
        }
        Self::matches_any(&self.excludes, text)
    }
}

#[derive(PartialEq, Clone, Copy)]
pub(crate) enum Focus { Components, Events, Detail, Filter, ComponentFilter }

#[derive(PartialEq, Clone)]
pub(crate) enum AppMode {
    Dashboard,
    GroupDetail,
    NewGroupName,
    RenameGroup,
    Main,
}

pub(crate) struct DashboardState {
    pub(crate) groups: Vec<DashboardGroup>,
    pub(crate) selected: usize,
    pub(crate) filter: String,
    pub(crate) filter_active: bool,
    pub(crate) new_group_name: String,
    pub(crate) new_group_proxies: Vec<NewProxyEntry>,
    pub(crate) error: Option<String>,
    pub(crate) rename_input: String,
    pub(crate) delete_confirm: bool,
    pub(crate) detail_proxies: Vec<NewProxyEntry>,
    pub(crate) detail_selected: usize,
    pub(crate) detail_group_name: String,
    pub(crate) detail_delete_confirm: bool,
    fuzzy_matcher: SkimMatcherV2,
}

#[derive(Clone)]
pub(crate) struct DashboardGroup {
    pub(crate) name: String,
    pub(crate) proxies: Vec<String>, // proxy names for display
}

#[derive(Clone)]
pub(crate) struct NewProxyEntry {
    pub(crate) name: String,
    pub(crate) protocol: String,
    pub(crate) listen: String,
    pub(crate) remote: String,
    pub(crate) mode: String,
    pub(crate) interface: String,
}

impl DashboardState {
    pub(crate) fn load(group_dir: &std::path::Path, main_config: &std::path::Path) -> Self {
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

    pub(crate) fn filtered_indices(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            (0..self.groups.len()).collect()
        } else {
            self.groups.iter().enumerate().filter(|(_, g)| {
                self.fuzzy_matcher.fuzzy_match(&g.name, &self.filter).is_some()
            }).map(|(i, _)| i).collect()
        }
    }
}

pub(crate) const PROTOCOLS: &[&str] = &["redis", "mysql", "postgres", "amqp", "mongodb", "http", "memcached", "kafka"];

pub(crate) fn default_port(protocol: &str) -> &'static str {
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
pub(crate) struct ProxyForm {
    /// [name, listen_host, listen_port, remote_host, remote_port, interface]
    pub(crate) inputs: [tui_input::Input; 6],
    pub(crate) active_field: usize,
    pub(crate) editing_idx: Option<usize>,
    pub(crate) protocol_idx: usize,
    pub(crate) mode_idx: usize, // 0=proxy, 1=capture
    pub(crate) error: Option<String>,
    /// Existing listen addr for edit mode (reused on save)
    pub(crate) existing_listen: Option<String>,
}

pub(crate) const MODES: &[&str] = &["proxy", "capture"];
pub(crate) const DEFAULT_IFACE: &str = if cfg!(target_os = "macos") { "lo0" } else { "lo" };

impl ProxyForm {
    /// Map active_field (row index) to fields[] index.
    pub(crate) fn field_idx(&self) -> Option<usize> {
        let is_capture = self.mode_idx == 1;
        if self.editing_idx.is_some() && !is_capture {
            match self.active_field {
                0 => Some(0),
                1 | 2 => None,
                3 => Some(1),
                4 => Some(2),
                5 => Some(3),
                6 => Some(4),
                _ => None,
            }
        } else if is_capture {
            match self.active_field {
                0 => Some(0),
                1 | 2 => None,
                3 => Some(3),
                4 => Some(4),
                5 => Some(5),
                _ => None,
            }
        } else {
            match self.active_field {
                0 => Some(0),
                1 | 2 => None,
                3 => Some(3),
                4 => Some(4),
                _ => None,
            }
        }
    }

    pub(crate) fn row_count(&self) -> usize {
        let is_capture = self.mode_idx == 1;
        if is_capture {
            6
        } else if self.editing_idx.is_some() {
            7
        } else {
            5
        }
    }

    pub(crate) fn from_entry(entry: &NewProxyEntry) -> Self {
        let protocol_idx = PROTOCOLS.iter().position(|&p| p == entry.protocol).unwrap_or(0);
        let mode_idx = if entry.mode == "capture" { 1 } else { 0 };
        let (rh, rp) = crate::helpers::split_addr(&entry.remote);
        let (lh, lp) = crate::helpers::split_addr(&entry.listen);
        Self {
            inputs: [
                tui_input::Input::new(entry.name.clone()),
                tui_input::Input::new(lh),
                tui_input::Input::new(lp),
                tui_input::Input::new(rh),
                tui_input::Input::new(rp),
                tui_input::Input::new(entry.interface.clone()),
            ],
            active_field: 0,
            editing_idx: None,
            protocol_idx,
            mode_idx,
            error: None,
            existing_listen: Some(entry.listen.clone()),
        }
    }
}

pub(crate) fn auto_assign_listen_port(protocol: &str) -> String {
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

pub(crate) struct GroupPicker {
    pub(crate) groups: Vec<String>,
    pub(crate) selected: usize,
}

/// Per-component aggregate statistics
#[derive(Default)]
pub(crate) struct ComponentStats {
    pub(crate) count: u64,
    pub(crate) error_count: u64,
    pub(crate) latency_sum: Duration,
    pub(crate) latency_min: Duration,
    pub(crate) latency_max: Duration,
    pub(crate) latencies: Vec<Duration>,
    pub(crate) first_event: Option<SystemTime>,
    pub(crate) last_event: Option<SystemTime>,
}

impl ComponentStats {
    pub(crate) fn record(&mut self, ev: &ProxyEvent) {
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

    pub(crate) fn avg_latency(&self) -> Duration {
        let n = self.latencies.len() as u64;
        if n == 0 { return Duration::ZERO; }
        self.latency_sum / n as u32
    }

    pub(crate) fn p95_latency(&self) -> Duration {
        if self.latencies.is_empty() { return Duration::ZERO; }
        let mut sorted = self.latencies.clone();
        sorted.sort();
        let idx = (sorted.len() as f64 * 0.95) as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    pub(crate) fn qps(&self) -> f64 {
        match (self.first_event, self.last_event) {
            (Some(first), Some(last)) => {
                let elapsed = last.duration_since(first).unwrap_or(Duration::ZERO).as_secs_f64();
                if elapsed < 1.0 { self.count as f64 } else { self.count as f64 / elapsed }
            }
            _ => 0.0,
        }
    }

    pub(crate) fn error_rate(&self) -> f64 {
        if self.count == 0 { return 0.0; }
        self.error_count as f64 / self.count as f64 * 100.0
    }
}

pub(crate) struct App {
    pub(crate) events: Vec<ProxyEvent>,
    pub(crate) selected: usize,
    pub(crate) detail_scroll: u16,
    pub(crate) focus: Focus,
    pub(crate) components: Vec<ComponentInfo>,
    pub(crate) component_idx: Option<usize>,
    pub(crate) filter: String,
    pub(crate) pending_keys: String,
    pub(crate) leader_active: bool,
    pub(crate) show_leader_menu: bool,
    pub(crate) help_active: bool,
    pub(crate) confirm_quit: bool,
    pub(crate) quit_confirm_enabled: bool,
    pub(crate) visual_mode: bool,
    pub(crate) visual_anchor: usize,
    pub(crate) theme: crate::theme::Theme,
    pub(crate) paused: bool,
    pub(crate) paused_buffer: Vec<ProxyEvent>,
    pub(crate) follow: bool,
    pub(crate) exclude_matchers: std::collections::HashMap<String, ExcludeMatcher>,
    pub(crate) event_format: EventFormat,
    pub(crate) latency_threshold_ms: Option<f64>,
    pub(crate) fuzzy_filter: bool,
    pub(crate) proxy_form: Option<ProxyForm>,
    pub(crate) delete_confirm_idx: Option<usize>,
    pub(crate) info_popup_idx: Option<usize>,
    pub(crate) component_filter: String,
    pub(crate) config_path: PathBuf,
    pub(crate) group_dir: Option<PathBuf>,
    pub(crate) active_group: Option<String>,
    pub(crate) group_picker: Option<GroupPicker>,
    pub(crate) proxy_change_tx: Option<broadcast::Sender<ProxyChange>>,
    pub(crate) mode: AppMode,
    pub(crate) dashboard: DashboardState,
    pub(crate) main_config_path: PathBuf,
    pub(crate) status_map: StatusMap,
    pub(crate) component_stats: std::collections::HashMap<String, ComponentStats>,
    pub(crate) fuzzy_matcher: SkimMatcherV2,
    pub(crate) dirty: bool,
    pub(crate) cached_filtered_indices: Vec<usize>,
    pub(crate) cached_filter_key: Option<(usize, String, Option<usize>, bool)>,
    pub(crate) preview: bool,
}

impl App {
    pub(crate) fn filtered_events(&self) -> Vec<(usize, &ProxyEvent, Vec<usize>)> {
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

    pub(crate) fn refresh_filter_cache(&mut self) {
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

pub(crate) fn filtered_component_indices(app: &App) -> Vec<usize> {
    if app.component_filter.is_empty() {
        (0..app.components.len()).collect()
    } else {
        app.components.iter().enumerate().filter(|(_, c)| {
            let target = format!("{} {} {}", c.name, c.listen, c.listen);
            app.fuzzy_matcher.fuzzy_match(&target, &app.component_filter).is_some()
        }).map(|(i, _)| i).collect()
    }
}

/// Event line format template.
/// Syntax: `%field` or `%{width}field` for fixed width.
/// Positive width = right-aligned, negative = left-aligned.
/// Fields: index, time, component, command, latency, process
#[derive(Debug, Clone)]
pub(crate) struct EventFormat {
    pub(crate) segments: Vec<FormatSegment>,
}

#[derive(Debug, Clone)]
pub(crate) enum FormatSegment {
    Literal(String),
    Field { name: String, width: Option<i32> },
}

impl EventFormat {
    pub(crate) fn parse(template: &str) -> Self {
        let mut segments = Vec::new();
        let mut chars = template.chars().peekable();
        let mut literal = String::new();

        while let Some(c) = chars.next() {
            if c == '%' {
                if !literal.is_empty() {
                    segments.push(FormatSegment::Literal(std::mem::take(&mut literal)));
                }
                let width = if chars.peek() == Some(&'{') {
                    chars.next();
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

    pub(crate) fn default_format() -> Self {
        Self::parse("%{5}index %time [%{-12}component] %command (%latency)")
    }
}

/// Config structure for hot-reload (only the parts we can reload)
#[derive(Debug, Deserialize)]
pub(crate) struct ReloadableConfig {
    #[serde(default)]
    pub(crate) proxy: Vec<ReloadableProxy>,
    #[serde(default)]
    pub(crate) exclude: std::collections::HashMap<String, ReloadableExclude>,
    #[serde(default)]
    pub(crate) theme: Option<String>,
    #[serde(default)]
    pub(crate) theme_overrides: Option<ThemeConfig>,
    #[serde(default)]
    pub(crate) event_format: Option<String>,
    #[serde(default)]
    pub(crate) latency_threshold_ms: Option<f64>,
    #[serde(default = "default_true")]
    pub(crate) fuzzy_filter: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReloadableProxy {
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) protocol: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub(crate) listen: String,
    #[serde(default)]
    pub(crate) remote: String,
    #[serde(default)]
    pub(crate) mode: Option<String>,
    #[serde(default)]
    pub(crate) interface: Option<String>,
    #[serde(default)]
    pub(crate) exclude: Option<ReloadableExclude>,
    #[serde(default)]
    pub(crate) include: Option<ReloadableExclude>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReloadableExclude {
    pub(crate) patterns: Vec<String>,
    #[serde(default)]
    pub(crate) case_sensitive: bool,
    #[serde(default)]
    pub(crate) regex: bool,
}

pub(crate) fn default_true() -> bool { true }

/// Notification sent to TUI when proxies change via hot-reload
#[derive(Debug, Clone)]
pub enum ProxyChange {
    Added(ComponentInfo),
    Removed(String),
    SwitchGroup(PathBuf),
    StopAll,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    // ─── EventFormat ────────────────────────────────────────────────────

    #[test]
    fn test_event_format_parse_simple() {
        let fmt = EventFormat::parse("%time %command");
        assert_eq!(fmt.segments.len(), 3); // time, space, command
    }

    #[test]
    fn test_event_format_parse_with_width() {
        let fmt = EventFormat::parse("%{5}index %{10}time");
        match &fmt.segments[0] {
            FormatSegment::Field { name, width } => {
                assert_eq!(name, "index");
                assert_eq!(*width, Some(5));
            }
            _ => panic!("expected field"),
        }
    }

    #[test]
    fn test_event_format_parse_negative_width() {
        let fmt = EventFormat::parse("%{-12}component");
        match &fmt.segments[0] {
            FormatSegment::Field { name, width } => {
                assert_eq!(name, "component");
                assert_eq!(*width, Some(-12));
            }
            _ => panic!("expected field"),
        }
    }

    #[test]
    fn test_event_format_parse_literal() {
        let fmt = EventFormat::parse("hello world");
        assert_eq!(fmt.segments.len(), 1);
        match &fmt.segments[0] {
            FormatSegment::Literal(s) => assert_eq!(s, "hello world"),
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_event_format_parse_mixed() {
        let fmt = EventFormat::parse("%{5}index %time [%{-12}component] %command (%latency)");
        // Should have segments for each field and literal
        assert!(fmt.segments.len() > 5);
    }

    #[test]
    fn test_event_format_default() {
        let fmt = EventFormat::default_format();
        assert!(!fmt.segments.is_empty());
    }

    // ─── ExcludeMatcher ─────────────────────────────────────────────────

    #[test]
    fn test_exclude_matcher_empty() {
        let m = ExcludeMatcher::new(None, None);
        assert!(m.is_noop());
        assert!(!m.is_excluded("PING"));
    }

    #[test]
    fn test_exclude_matcher_plain() {
        let cfg = vec![ExcludeConfig {
            patterns: vec!["PING".into(), "INFO".into()],
            case_sensitive: false,
            regex: false,
        }];
        let m = ExcludeMatcher::new(Some(&cfg), None);
        assert!(!m.is_noop());
        assert!(m.is_excluded("PING"));
        assert!(m.is_excluded("ping")); // case insensitive
        assert!(!m.is_excluded("SET key value"));
    }

    #[test]
    fn test_exclude_matcher_case_sensitive() {
        let cfg = vec![ExcludeConfig {
            patterns: vec!["PING".into()],
            case_sensitive: true,
            regex: false,
        }];
        let m = ExcludeMatcher::new(Some(&cfg), None);
        assert!(m.is_excluded("PING"));
        assert!(!m.is_excluded("ping"));
    }

    #[test]
    fn test_exclude_matcher_regex() {
        let cfg = vec![ExcludeConfig {
            patterns: vec!["^SELECT 1$".into()],
            case_sensitive: false,
            regex: true,
        }];
        let m = ExcludeMatcher::new(Some(&cfg), None);
        assert!(m.is_excluded("SELECT 1"));
        assert!(!m.is_excluded("SELECT 1 FROM dual"));
    }

    #[test]
    fn test_exclude_matcher_include_overrides() {
        let exclude = vec![ExcludeConfig {
            patterns: vec!["PING".into()],
            case_sensitive: false,
            regex: false,
        }];
        let include = ExcludeConfig {
            patterns: vec!["PING".into()],
            case_sensitive: false,
            regex: false,
        };
        let m = ExcludeMatcher::new(Some(&exclude), Some(&include));
        assert!(!m.is_excluded("PING")); // include overrides
        assert!(!m.is_excluded("SET key"));
    }

    // ─── ComponentStats ─────────────────────────────────────────────────

    fn make_event(latency_ms: u64, response: &str) -> ProxyEvent {
        ProxyEvent {
            timestamp: SystemTime::now(),
            component: "test".into(),
            protocol: ocular_protocol::Protocol::Redis,
            command: "CMD".into(),
            full_command: "CMD".into(),
            response: response.into(),
            response_detail: String::new(),
            latency: Duration::from_millis(latency_ms),
            process: None,
            src: None,
            dest: None,
            system: false,
        }
    }

    #[test]
    fn test_component_stats_record() {
        let mut stats = ComponentStats::default();
        stats.record(&make_event(5, "OK"));
        stats.record(&make_event(10, "OK"));
        stats.record(&make_event(15, "OK"));
        assert_eq!(stats.count, 3);
        assert_eq!(stats.error_count, 0);
    }

    #[test]
    fn test_component_stats_errors() {
        let mut stats = ComponentStats::default();
        stats.record(&make_event(5, "OK"));
        stats.record(&make_event(10, "ERR timeout"));
        stats.record(&make_event(3, "ERROR bad"));
        assert_eq!(stats.error_count, 2);
    }

    #[test]
    fn test_component_stats_error_rate() {
        let mut stats = ComponentStats::default();
        stats.record(&make_event(5, "OK"));
        stats.record(&make_event(10, "ERR"));
        assert!((stats.error_rate() - 50.0).abs() < 0.01);
    }

    #[test]
    fn test_component_stats_error_rate_empty() {
        let stats = ComponentStats::default();
        assert_eq!(stats.error_rate(), 0.0);
    }

    #[test]
    fn test_component_stats_avg_latency() {
        let mut stats = ComponentStats::default();
        stats.record(&make_event(10, "OK"));
        stats.record(&make_event(20, "OK"));
        stats.record(&make_event(30, "OK"));
        let avg = stats.avg_latency();
        assert_eq!(avg.as_millis(), 20);
    }

    #[test]
    fn test_component_stats_avg_latency_empty() {
        let stats = ComponentStats::default();
        assert_eq!(stats.avg_latency(), Duration::ZERO);
    }

    #[test]
    fn test_component_stats_p95() {
        let mut stats = ComponentStats::default();
        for i in 1..=100 {
            stats.record(&make_event(i, "OK"));
        }
        let p95 = stats.p95_latency();
        // P95 of 1..100 should be around 95ms
        assert!(p95.as_millis() >= 94 && p95.as_millis() <= 96);
    }

    #[test]
    fn test_component_stats_p95_empty() {
        let stats = ComponentStats::default();
        assert_eq!(stats.p95_latency(), Duration::ZERO);
    }

    #[test]
    fn test_component_stats_system_event_skipped() {
        let mut stats = ComponentStats::default();
        let mut ev = make_event(5, "ERR");
        ev.system = true;
        stats.record(&ev);
        assert_eq!(stats.count, 1);
        assert_eq!(stats.error_count, 1);
        // But latency should not be recorded for system events
        assert_eq!(stats.latencies.len(), 0);
    }

    #[test]
    fn test_component_stats_min_max() {
        let mut stats = ComponentStats::default();
        stats.record(&make_event(3, "OK"));
        stats.record(&make_event(15, "OK"));
        stats.record(&make_event(7, "OK"));
        assert_eq!(stats.latency_min.as_millis(), 3);
        assert_eq!(stats.latency_max.as_millis(), 15);
    }
}
