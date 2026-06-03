use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use fuzzy_matcher::skim::SkimMatcherV2;
use ocular_proxy::{ProxyEvent, StatusMap};
use ratatui::prelude::*;
use std::io::stdout;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tokio::sync::broadcast;

mod types;
mod helpers;
mod config;
mod render;
mod theme;

pub use types::{ComponentInfo, ExcludeConfig, ProxyChange};
pub use theme::{Theme, ThemeConfig};

use types::*;
use helpers::*;
use config::*;
use render::{ui_dashboard, ui};

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
    preview: bool,
    skip_dashboard: bool,
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
        components: if preview || skip_dashboard { components.clone() } else { Vec::new() }, // pre-populate for preview or skip-dashboard; normal: wait for group selection
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
        mode: if preview || skip_dashboard { AppMode::Main } else { AppMode::Dashboard },
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
        preview,
    };

    let mut last_mtime = SystemTime::UNIX_EPOCH;
    let mut proxy_change_rx = proxy_change_rx;
    let mut last_tick = std::time::Instant::now();

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
                                        // Create empty group file and enter GroupDetail
                                        if let Some(ref gdir) = app.group_dir.clone() {
                                            let file = gdir.join(format!("{}.toml", name));
                                            let _ = std::fs::write(&file, "");
                                            app.dashboard = DashboardState::load(gdir, &app.main_config_path);
                                        }
                                        app.dashboard.detail_group_name = name;
                                        app.dashboard.detail_proxies = Vec::new();
                                        app.dashboard.detail_selected = 0;
                                        app.proxy_form = None;
                                        app.mode = AppMode::GroupDetail;
                                    }
                                }
                                KeyCode::Backspace => { app.dashboard.new_group_name.pop(); app.dashboard.error = None; }
                                KeyCode::Char(c) => { app.dashboard.new_group_name.push(c); app.dashboard.error = None; }
                                _ => {}
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
                                    KeyCode::Char('y') | KeyCode::Enter
                                        if app.dashboard.detail_selected < app.dashboard.detail_proxies.len() => {
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
                                        let name = form.inputs[0].value().trim().to_string();
                                        let remote_host = if form.inputs[3].value().is_empty() { "127.0.0.1" } else { form.inputs[3].value().trim() };
                                        let remote_port = if form.inputs[4].value().is_empty() { default_port(protocol) } else { form.inputs[4].value().trim() };
                                        if name.is_empty() {
                                            form.error = Some("name is required".into());
                                        } else if app.dashboard.detail_proxies.iter().any(|p| p.name == name)
                                            && form.editing_idx.is_none() {
                                            form.error = Some(format!("name \"{}\" already exists", name));
                                        } else {
                                            let is_capture = form.mode_idx == 1;
                                            let listen = if is_capture {
                                                String::new()
                                            } else if form.editing_idx.is_some() && (!form.inputs[1].value().is_empty() || !form.inputs[2].value().is_empty()) {
                                                let lh = if form.inputs[1].value().is_empty() { "127.0.0.1" } else { form.inputs[1].value().trim() };
                                                let lp = if form.inputs[2].value().is_empty() { "0" } else { form.inputs[2].value().trim() };
                                                format!("{}:{}", lh, lp)
                                            } else {
                                                form.existing_listen.clone().unwrap_or_else(|| auto_assign_listen_port(protocol))
                                            };
                                            let remote = format!("{}:{}", remote_host, remote_port);
                                            let mode_str = MODES[form.mode_idx].to_string();
                                            let iface = form.inputs[5].value().to_string();
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
                                        else if let Some(fi) = form.field_idx() { form.inputs[fi].handle(tui_input::InputRequest::DeletePrevChar); }
                                        form.error = None;
                                    }
                                    KeyCode::Char(c) => {
                                        if form.active_field == 1 {
                                            match c { 'h' | 'k' => form.protocol_idx = (form.protocol_idx + PROTOCOLS.len() - 1) % PROTOCOLS.len(), 'l' | 'j' => form.protocol_idx = (form.protocol_idx + 1) % PROTOCOLS.len(), _ => {} }
                                        } else if form.active_field == 2 {
                                            match c { 'h' | 'k' => form.mode_idx = (form.mode_idx + MODES.len() - 1) % MODES.len(), 'l' | 'j' => form.mode_idx = (form.mode_idx + 1) % MODES.len(), _ => {} }
                                            form.active_field = form.active_field.min(form.row_count() - 1);
                                        } else if let Some(fi) = form.field_idx() { form.inputs[fi].handle(tui_input::InputRequest::InsertChar(c)); }
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
                                    KeyCode::Char('j') | KeyCode::Down
                                        if app.dashboard.detail_selected + 1 < app.dashboard.detail_proxies.len() => {
                                            app.dashboard.detail_selected += 1;
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
                                    KeyCode::Char('d')
                                        if !app.dashboard.detail_proxies.is_empty() => {
                                            app.dashboard.detail_delete_confirm = true;
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
                            if !matcher.is_noop() {
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
            last_tick = std::time::Instant::now();
        } else if last_tick.elapsed() >= Duration::from_secs(1) {
            // Periodic redraw for status indicator cooldown
            terminal.draw(|f| ui(f, &mut app))?;
            last_tick = std::time::Instant::now();
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
                            let name = form.inputs[0].value().trim().to_string();
                            let remote_host = if form.inputs[3].value().is_empty() { "127.0.0.1" } else { form.inputs[3].value().trim() };
                            let remote_port = if form.inputs[4].value().is_empty() { default_port(protocol) } else { form.inputs[4].value().trim() };

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
                                    let listen_addr = if is_capture {
                                        String::new()
                                    } else if form.editing_idx.is_some() && (!form.inputs[1].value().is_empty() || !form.inputs[2].value().is_empty()) {
                                        let lh = if form.inputs[1].value().is_empty() { "127.0.0.1" } else { form.inputs[1].value().trim() };
                                        let lp = if form.inputs[2].value().is_empty() { "0" } else { form.inputs[2].value().trim() };
                                        format!("{}:{}", lh, lp)
                                    } else {
                                        form.existing_listen.clone().unwrap_or_else(|| auto_assign_listen_port(protocol))
                                    };
                                    let mode_str = MODES[form.mode_idx];
                                    let iface = if form.inputs[5].value().trim().is_empty() {
                                        DEFAULT_IFACE.to_string()
                                    } else { form.inputs[5].value().trim().to_string() };
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
                        KeyCode::Left => {
                            if let Some(fi) = form.field_idx() {
                                use tui_input::InputRequest as IR;
                                let req = if key.modifiers.contains(crossterm::event::KeyModifiers::SUPER) {
                                    IR::GoToStart
                                } else if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
                                    IR::GoToPrevWord
                                } else {
                                    IR::GoToPrevChar
                                };
                                form.inputs[fi].handle(req);
                            }
                        }
                        KeyCode::Right => {
                            if let Some(fi) = form.field_idx() {
                                use tui_input::InputRequest as IR;
                                let req = if key.modifiers.contains(crossterm::event::KeyModifiers::SUPER) {
                                    IR::GoToEnd
                                } else if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
                                    IR::GoToNextWord
                                } else {
                                    IR::GoToNextChar
                                };
                                form.inputs[fi].handle(req);
                            }
                        }
                        KeyCode::Backspace => {
                            if form.active_field == 1 {
                                form.protocol_idx = (form.protocol_idx + PROTOCOLS.len() - 1) % PROTOCOLS.len();
                            } else if form.active_field == 2 {
                                form.mode_idx = (form.mode_idx + MODES.len() - 1) % MODES.len();
                                form.active_field = form.active_field.min(form.row_count() - 1);
                            } else if let Some(fi) = form.field_idx() {
                                use tui_input::InputRequest as IR;
                                let req = if key.modifiers.contains(crossterm::event::KeyModifiers::SUPER) {
                                    IR::DeleteLine
                                } else if key.modifiers.contains(crossterm::event::KeyModifiers::ALT) {
                                    IR::DeletePrevWord
                                } else {
                                    IR::DeletePrevChar
                                };
                                form.inputs[fi].handle(req);
                            }
                            form.error = None;
                        }
                        KeyCode::Char(c) => {
                            if form.active_field == 1 {
                                match c {
                                    'h' | 'k' => form.protocol_idx = (form.protocol_idx + PROTOCOLS.len() - 1) % PROTOCOLS.len(),
                                    'l' | 'j' => form.protocol_idx = (form.protocol_idx + 1) % PROTOCOLS.len(),
                                    _ => {}
                                }
                            } else if form.active_field == 2 {
                                match c {
                                    'h' | 'k' => form.mode_idx = (form.mode_idx + MODES.len() - 1) % MODES.len(),
                                    'l' | 'j' => form.mode_idx = (form.mode_idx + 1) % MODES.len(),
                                    _ => {}
                                }
                                form.active_field = form.active_field.min(form.row_count() - 1);
                            } else if let Some(fi) = form.field_idx() {
                                form.inputs[fi].handle(tui_input::InputRequest::InsertChar(c));
                            }
                            form.error = None;
                        }
                        _ => {}
                    }
                    app.dirty = true;
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
                    app.dirty = true;
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
                    app.dirty = true;
                    continue;
                }

                if app.leader_active {
                    app.leader_active = false;
                    match key.code {
                        KeyCode::Char('j') => { app.focus = Focus::Detail; app.detail_scroll = 0; }
                        KeyCode::Char('k') => { app.focus = Focus::Events; }
                        KeyCode::Char('h') if !app.preview => { app.focus = Focus::Components; }
                        KeyCode::Char('l') if !app.preview => { app.focus = Focus::Events; }
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
                        KeyCode::Char(',') if !app.preview => {
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
                        KeyCode::Char('g') if !app.preview => {
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
                    app.dirty = true;
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => {
                        if preview {
                            break;
                        } else if app.quit_confirm_enabled { app.confirm_quit = true; } else {
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
                                    inputs: [
                                        tui_input::Input::new(ci.name.clone()),
                                        tui_input::Input::default(),
                                        tui_input::Input::default(),
                                        tui_input::Input::default(),
                                        tui_input::Input::default(),
                                        tui_input::Input::default(),
                                    ],
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
                                            form.inputs[3] = tui_input::Input::new(rh);
                                            form.inputs[4] = tui_input::Input::new(rp);
                                            let (lh, lp) = split_addr(&ci.listen);
                                            form.inputs[1] = tui_input::Input::new(lh);
                                            form.inputs[2] = tui_input::Input::new(lp);
                                            form.inputs[5] = tui_input::Input::new(p.interface.clone().unwrap_or_default());
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
