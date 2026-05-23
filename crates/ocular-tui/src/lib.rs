use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ocular_proxy::ProxyEvent;
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
            cfgs.iter().flat_map(|c| Self::compile_patterns(c)).collect()
        }).unwrap_or_default();
        Self {
            excludes: exclude_matchers,
            includes: include.map(|c| Self::compile_patterns(c)).unwrap_or_default(),
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
enum Focus { Components, Events, Detail, Filter }

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
    visual_mode: bool,
    visual_anchor: usize, // start of visual selection
    theme: Theme,
    paused: bool,
    exclude_matchers: std::collections::HashMap<String, ExcludeMatcher>,
    event_format: EventFormat,
}

impl App {
    fn filtered_events(&self) -> Vec<(usize, &ProxyEvent)> {
        self.events.iter().enumerate().filter(|(_, ev)| {
            if let Some(idx) = self.component_idx {
                if let Some(c) = self.components.get(idx) {
                    if ev.component != c.name { return false; }
                }
            }
            if !self.filter.is_empty() {
                let q = self.filter.to_lowercase();
                if !ev.component.to_lowercase().contains(&q)
                    && !ev.command.to_lowercase().contains(&q) {
                    return false;
                }
            }
            true
        }).collect()
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
}

#[derive(Debug, Deserialize)]
struct ReloadableProxy {
    name: String,
    #[serde(default)]
    protocol: String,
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

pub async fn run(
    mut rx: broadcast::Receiver<ProxyEvent>,
    components: Vec<ComponentInfo>,
    theme: Theme,
    config_path: PathBuf,
    event_format: Option<String>,
    show_leader_menu: bool,
) -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

    let exclude_matchers: std::collections::HashMap<String, ExcludeMatcher> = components.iter()
        .filter(|c| c.exclude.is_some() || c.include.is_some())
        .map(|c| (c.name.clone(), ExcludeMatcher::new(c.exclude.as_ref(), c.include.as_ref())))
        .collect();

    let fmt = event_format.as_deref().map(EventFormat::parse).unwrap_or_else(EventFormat::default_format);

    let mut app = App {
        events: Vec::new(),
        selected: 0,
        detail_scroll: 0,
        focus: Focus::Events,
        components,
        component_idx: None,
        filter: String::new(),
        pending_keys: String::new(),
        leader_active: false,
        show_leader_menu,
        help_active: false,
        confirm_quit: false,
        visual_mode: false,
        visual_anchor: 0,
        theme,
        paused: false,
        exclude_matchers,
        event_format: fmt,
    };

    let mut last_mtime = std::fs::metadata(&config_path).ok()
        .and_then(|m| m.modified().ok())
        .unwrap_or(SystemTime::UNIX_EPOCH);

    loop {
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

        while let Ok(ev) = rx.try_recv() {
            if app.paused { continue; }
            if let Some(matcher) = app.exclude_matchers.get(&ev.component) {
                if matcher.is_excluded(&ev.command) { continue; }
            }
            app.events.push(ev);
            if app.focus == Focus::Events && app.filter.is_empty() {
                let filtered = app.filtered_events();
                app.selected = filtered.len().saturating_sub(1);
            }
        }

        terminal.draw(|f| ui(f, &mut app))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press { continue; }

                // Ctrl+C: force quit regardless of state
                if key.code == KeyCode::Char('c') && key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
                    break;
                }

                if app.focus == Focus::Filter {
                    match key.code {
                        KeyCode::Esc => { app.focus = Focus::Events; }
                        KeyCode::Enter => { app.focus = Focus::Events; app.selected = 0; }
                        KeyCode::Backspace => { app.filter.pop(); app.selected = 0; }
                        KeyCode::Char(c) => { app.filter.push(c); app.selected = 0; }
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
                        KeyCode::Char('y') => break,
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
                        KeyCode::Char('c') => { app.events.clear(); app.selected = 0; }
                        KeyCode::Char('p') => { app.paused = !app.paused; }
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
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => { app.confirm_quit = true; }
                    KeyCode::Char('?') => { app.help_active = !app.help_active; }
                    KeyCode::Char(' ') => {
                        app.pending_keys.clear();
                        app.leader_active = true;
                    }
                    KeyCode::Char('/') => {
                        app.pending_keys.clear();
                        app.focus = Focus::Filter;
                    }
                    KeyCode::Esc => {
                        app.pending_keys.clear();
                        if app.focus == Focus::Detail {
                            app.focus = Focus::Events;
                        } else if !app.filter.is_empty() {
                            app.filter.clear();
                            app.selected = 0;
                            app.focus = Focus::Events;
                        } else if app.visual_mode {
                            app.visual_mode = false;
                        } else {
                            app.component_idx = None;
                            app.selected = 0;
                            app.focus = Focus::Events;
                        }
                    }
                    KeyCode::Tab => {
                        app.pending_keys.clear();
                        app.focus = match app.focus {
                            Focus::Components => Focus::Events,
                            Focus::Events => Focus::Detail,
                            Focus::Detail => Focus::Components,
                            Focus::Filter => Focus::Events,
                        };
                        app.detail_scroll = 0;
                    }
                    KeyCode::BackTab => {
                        app.pending_keys.clear();
                        app.focus = match app.focus {
                            Focus::Components => Focus::Detail,
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
                        let max = app.filtered_events().len().saturating_sub(1);
                        app.selected = max;
                        app.detail_scroll = 0;
                    }
                    KeyCode::Char('G') if app.focus == Focus::Detail => {
                        app.pending_keys.clear();
                        app.detail_scroll = u16::MAX;
                    }
                    KeyCode::Char('g') if app.focus == Focus::Events => {
                        if app.pending_keys.ends_with('g') {
                            let num_str: String = app.pending_keys.chars().take_while(|c| c.is_ascii_digit()).collect();
                            let max = app.filtered_events().len().saturating_sub(1);
                            if num_str.is_empty() {
                                app.selected = 0;
                            } else if let Ok(n) = num_str.parse::<usize>() {
                                app.selected = n.saturating_sub(1).min(max);
                            }
                            app.pending_keys.clear();
                            app.detail_scroll = 0;
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
                        if let Some((_, ev)) = filtered.get(app.selected) {
                            let detail_content = if ev.protocol == ocular_protocol::Protocol::Mysql || ev.protocol == ocular_protocol::Protocol::Postgres {
                                format!("{}\n\n-- Response: {}\n-- Latency: {}\n\n{}",
                                    ev.full_command, ev.response,
                                    format_latency(&ev.latency), ev.response_detail)
                            } else {
                                format!("{}\n\n# Response: {}\n# Latency: {}",
                                    ev.full_command, ev.response, format_latency(&ev.latency))
                            };
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
                                app.component_idx = match app.component_idx {
                                    None => Some(app.components.len().saturating_sub(1)),
                                    Some(0) => None,
                                    Some(i) => Some(i - 1),
                                };
                                app.selected = 0;
                            }
                            Focus::Events => { app.selected = app.selected.saturating_sub(1); app.detail_scroll = 0; }
                            Focus::Detail => { app.detail_scroll = app.detail_scroll.saturating_sub(1); }
                            _ => {}
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.pending_keys.clear();
                        match app.focus {
                            Focus::Components => {
                                app.component_idx = match app.component_idx {
                                    None => { if !app.components.is_empty() { Some(0) } else { None } }
                                    Some(i) => {
                                        if i + 1 < app.components.len() { Some(i + 1) } else { None }
                                    }
                                };
                                app.selected = 0;
                            }
                            Focus::Events => {
                                let max = app.filtered_events().len().saturating_sub(1);
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
                    _ => { app.pending_keys.clear(); }
                }
            }
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

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
    let comp_focused = app.focus == Focus::Components;
    let items: Vec<ListItem> = std::iter::once(ListItem::new(
        if app.component_idx.is_none() { " ● ALL".to_string() } else { "   ALL".to_string() }
    ).style(if app.component_idx.is_none() && comp_focused {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    } else if app.component_idx.is_none() {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }))
    .chain(app.components.iter().enumerate().map(|(i, c)| {
        let selected = app.component_idx == Some(i);
        let prefix = if selected { " ●" } else { "  " };
        let style = if selected && comp_focused {
            Style::default().bg(Color::DarkGray).fg(Color::White)
        } else if selected {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        let addr_style = if selected && comp_focused {
            Style::default().bg(Color::DarkGray).fg(Color::Rgb(160, 160, 160))
        } else {
            Style::default().fg(Color::Rgb(100, 100, 100))
        };
        let count = app.events.iter().filter(|ev| ev.component == c.name).count();
        ListItem::new(Line::from(vec![
            Span::styled(format!("{} {}", prefix, c.name), style),
            Span::styled(format!(" {}", count), addr_style),
            Span::styled(format!(" ({})", c.listen), addr_style),
        ])).style(style)
    })).collect();
    let comp_border = if comp_focused { Style::default().fg(Color::Cyan) } else { Style::default().fg(Color::DarkGray) };
    let left = List::new(items)
        .block(Block::default().borders(Borders::TOP).border_style(comp_border).title(format!(" Ocular v{} ", env!("CARGO_PKG_VERSION"))));
    f.render_widget(left, chunks[0]);

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
        .map(|(idx, (_orig_idx, ev))| {
            let time = format_time(&ev.timestamp);
            let lat = format_latency(&ev.latency);
            let spans: Vec<Span> = app.event_format.segments.iter().map(|seg| {
                match seg {
                    FormatSegment::Literal(s) => Span::raw(s.clone()),
                    FormatSegment::Field { name, width } => {
                        let (raw, style) = match name.as_str() {
                            "index" => (format!("{}", idx + 1), theme.line_number),
                            "time" => (time.clone(), theme.timestamp),
                            "component" => (format!("{}", ev.component), theme.component_style(&ev.component)),
                            "command" => (ev.command.clone(), theme.command),
                            "latency" => (format!("{}", lat), theme.latency),
                            "process" => (ev.process.clone().unwrap_or_default(), theme.latency),
                            _ => (String::new(), Style::default()),
                        };
                        let formatted = match width {
                            Some(w) if *w > 0 => format!("{:>width$}", raw, width = *w as usize),
                            Some(w) if *w < 0 => format!("{:<width$}", raw, width = (-*w) as usize),
                            _ => raw,
                        };
                        Span::styled(formatted, style)
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
    title_spans.push(Span::raw(format!("{}{} ", filter_info, count_info)));
    let event_list = List::new(event_items)
        .block(Block::default().borders(Borders::TOP).border_style(events_border)
            .title(Line::from(title_spans)));
    f.render_widget(event_list, right[0]);

    // Detail panel
    let detail_focused = app.focus == Focus::Detail;
    let selected_event = filtered.get(app.selected).map(|(_, ev)| *ev);
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
                for rd in ev.response_detail.lines() {
                    lines.push(Line::from(rd.to_string()));
                }
            }
        }

        // Build metadata line
        let mut meta_parts: Vec<Span> = Vec::new();
        meta_parts.push(Span::raw(format!("{}  ", format_time(&ev.timestamp))));
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
        .map(|l| ((l.chars().count().max(1) + detail_view_width - 1) / detail_view_width) as u16)
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
        let right_line = Line::from(vec![mode_span]);
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
            Line::from(vec![Span::styled(" p", Style::default().fg(Color::Cyan)), Span::raw("  → Pause/resume stream")]),
            Line::from(vec![Span::styled(" ,", Style::default().fg(Color::Cyan)), Span::raw("  → Edit config")]),
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

fn get_selected_commands(filtered: &[(usize, &ProxyEvent)], app: &App) -> String {
    if app.visual_mode {
        let lo = app.visual_anchor.min(app.selected);
        let hi = app.visual_anchor.max(app.selected);
        filtered.iter()
            .enumerate()
            .filter(|(idx, _)| *idx >= lo && *idx <= hi)
            .map(|(_, (_, ev))| ev.full_command.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        filtered.get(app.selected)
            .map(|(_, ev)| ev.full_command.clone())
            .unwrap_or_default()
    }
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
