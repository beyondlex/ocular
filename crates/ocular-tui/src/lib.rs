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
use std::io::stdout;
use std::time::Duration;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct ComponentInfo {
    pub name: String,
    pub listen: String,
}

#[derive(PartialEq, Clone, Copy)]
enum Focus { Components, Events, Detail, Filter }

struct App {
    events: Vec<ProxyEvent>,
    selected: usize,
    detail_scroll: u16,
    focus: Focus,
    components: Vec<ComponentInfo>,
    component_idx: Option<usize>, // None = all, Some(i) = filter by component i
    filter: String,
    pending_keys: String, // for number + gg jumps
    leader_active: bool,  // space leader menu open
}

impl App {
    fn filtered_events(&self) -> Vec<(usize, &ProxyEvent)> {
        self.events.iter().enumerate().filter(|(_, ev)| {
            // Component filter
            if let Some(idx) = self.component_idx {
                if let Some(c) = self.components.get(idx) {
                    if ev.component != c.name { return false; }
                }
            }
            // Text filter
            if !self.filter.is_empty() {
                let q = self.filter.to_lowercase();
                if !ev.component.to_lowercase().contains(&q)
                    && !ev.summary.to_lowercase().contains(&q) {
                    return false;
                }
            }
            true
        }).collect()
    }
}

pub async fn run(
    mut rx: broadcast::Receiver<ProxyEvent>,
    components: Vec<ComponentInfo>,
) -> Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

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
    };

    loop {
        while let Ok(ev) = rx.try_recv() {
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

                if app.leader_active {
                    app.leader_active = false;
                    match key.code {
                        KeyCode::Char('j') => { app.focus = Focus::Detail; app.detail_scroll = 0; }
                        KeyCode::Char('k') => { app.focus = Focus::Events; }
                        KeyCode::Char('h') => { app.focus = Focus::Components; }
                        KeyCode::Char('l') => { app.focus = Focus::Events; }
                        KeyCode::Char('c') => { app.events.clear(); app.selected = 0; }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => break,
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
                        if !app.filter.is_empty() {
                            app.filter.clear();
                        } else {
                            app.component_idx = None;
                        }
                        app.selected = 0;
                        app.focus = Focus::Events;
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
                            // gg or Ngg
                            let num_str: String = app.pending_keys.chars().take_while(|c| c.is_ascii_digit()).collect();
                            let max = app.filtered_events().len().saturating_sub(1);
                            if num_str.is_empty() {
                                app.selected = 0; // gg = go to first
                            } else if let Ok(n) = num_str.parse::<usize>() {
                                app.selected = n.saturating_sub(1).min(max); // Ngg = go to line N
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
                        if let Some((_, ev)) = filtered.get(app.selected) {
                            let text = if ev.direction == ocular_protocol::Direction::Request {
                                ocular_protocol::extract_full_command(ev.protocol, &ev.raw)
                                    .unwrap_or_else(|| ev.summary.clone())
                            } else {
                                ev.summary.clone()
                            };
                            copy_to_clipboard(&text);
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

fn format_latency(d: &Duration) -> String {
    let us = d.as_micros();
    if us < 1000 {
        format!("{}μs", us)
    } else if us < 1_000_000 {
        format!("{:.1}ms", us as f64 / 1000.0)
    } else {
        format!("{:.2}s", d.as_secs_f64())
    }
}

/// Sanitize raw bytes for safe terminal display.
/// Replaces control chars with escape notation, truncates to max_bytes.
fn sanitize_raw(raw: &[u8], max_bytes: usize) -> String {
    let truncated = raw.len() > max_bytes;
    let slice = &raw[..raw.len().min(max_bytes)];
    let mut out = String::with_capacity(slice.len());
    for &b in slice {
        match b {
            b'\r' => out.push_str("\\r"),
            b'\n' => { out.push_str("\\n"); out.push('\n'); }
            b'\t' => out.push_str("\\t"),
            0x00 => out.push_str("\\0"),
            0x20..=0x7e => out.push(b as char),
            _ => { out.push_str(&format!("\\x{:02x}", b)); }
        }
    }
    if truncated {
        out.push_str("\n... (truncated)");
    }
    out
}

fn ui(f: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
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
        ListItem::new(format!("{} 🟢 {} ({})", prefix, c.name, c.listen)).style(style)
    })).collect();
    let comp_border = if comp_focused { Style::default().fg(Color::Cyan) } else { Style::default() };
    let left = List::new(items)
        .block(Block::default().borders(Borders::ALL).border_style(comp_border).title(" Components "));
    f.render_widget(left, chunks[0]);

    // Right: vertical split
    let right = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[1]);

    // Event stream (filtered)
    let filtered = app.filtered_events();
    let events_focused = app.focus == Focus::Events;
    let visible_height = right[0].height.saturating_sub(2) as usize;
    // Ensure selected item is always within the visible window with margin
    let scroll_margin: usize = 3;
    let visible_start = if app.selected + scroll_margin < visible_height {
        0
    } else {
        app.selected + scroll_margin - visible_height + 1
    };
    let event_items: Vec<ListItem> = filtered.iter().enumerate()
        .skip(visible_start)
        .take(visible_height)
        .map(|(idx, (_orig_idx, ev))| {
            let arrow = match ev.direction {
                ocular_protocol::Direction::Request => "→",
                ocular_protocol::Direction::Response => "←",
            };
            let time = format_time(&ev.timestamp);
            let lat = ev.latency.as_ref().map(|d| format!(" ({})", format_latency(d))).unwrap_or_default();
            let line = format!(" {:>5} {} {} [{}] {}{}", idx + 1, time, arrow, ev.component, ev.summary, lat);
            let style = if idx == app.selected {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        }).collect();
    let filter_info = if app.filter.is_empty() {
        String::new()
    } else {
        format!(" [filter: {}]", app.filter)
    };
    let count_info = format!(" ({}/{})", filtered.len(), app.events.len());
    let events_border = if events_focused { Style::default().fg(Color::Cyan) } else { Style::default() };
    let event_list = List::new(event_items)
        .block(Block::default().borders(Borders::ALL).border_style(events_border)
            .title(format!(" Events{}{} ", filter_info, count_info)));
    f.render_widget(event_list, right[0]);

    // Detail panel
    let detail_focused = app.focus == Focus::Detail;
    let selected_event = filtered.get(app.selected).map(|(_, ev)| *ev);
    let detail = if let Some(ev) = selected_event {
        let lat_str = ev.latency.as_ref().map(|d| format_latency(d)).unwrap_or_else(|| "-".into());
        // For responses, show parsed result instead of raw bytes
        let body = if ev.direction == ocular_protocol::Direction::Response {
            if let Some(formatted) = ocular_protocol::format_response_detail(ev.protocol, &ev.raw) {
                formatted
            } else {
                sanitize_raw(&ev.raw, 512)
            }
        } else {
            sanitize_raw(&ev.raw, 512)
        };
        format!("Time:      {}\nComponent: {}\nDirection: {:?}\nLatency:   {}\nCommand:   {}\n\n{}",
            format_time(&ev.timestamp), ev.component, ev.direction, lat_str,
            ev.summary, body)
    } else {
        "No events yet. Waiting for traffic...".to_string()
    };
    let detail_border = if detail_focused { Style::default().fg(Color::Cyan) } else { Style::default() };
    let title = if detail_focused { " Detail (j/k scroll) " } else { " Detail " };
    let detail_view_width = right[1].width.saturating_sub(2).max(1) as usize;
    // Estimate total rendered lines (accounting for line wrapping)
    let wrapped_lines: u16 = detail.lines()
        .map(|l| {
            let w = l.chars().count().max(1);
            (((w + detail_view_width - 1) / detail_view_width) as u16).max(1)
        })
        .sum();
    let max_scroll = wrapped_lines.saturating_sub(1);
    app.detail_scroll = app.detail_scroll.min(max_scroll);
    let detail_widget = Paragraph::new(detail)
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0))
        .block(Block::default().borders(Borders::ALL).border_style(detail_border).title(title));
    f.render_widget(detail_widget, right[1]);

    // Bottom status bar
    let status = if app.focus == Focus::Filter {
        Span::styled(format!("/{}", app.filter), Style::default().fg(Color::Yellow))
    } else {
        Span::raw(" Tab cycle panels │ / filter │ j/k navigate │ Esc clear │ q quit")
    };
    f.render_widget(Paragraph::new(Line::from(status)), outer[1]);

    // Leader key floating menu
    if app.leader_active {
        let menu_lines = vec![
            Line::from(Span::styled(" Space Leader Menu", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
            Line::from(""),
            Line::from(vec![Span::styled(" h", Style::default().fg(Color::Cyan)), Span::raw("  → Components panel")]),
            Line::from(vec![Span::styled(" j", Style::default().fg(Color::Cyan)), Span::raw("  → Detail panel")]),
            Line::from(vec![Span::styled(" k", Style::default().fg(Color::Cyan)), Span::raw("  → Events panel")]),
            Line::from(vec![Span::styled(" l", Style::default().fg(Color::Cyan)), Span::raw("  → Events panel")]),
            Line::from(vec![Span::styled(" c", Style::default().fg(Color::Cyan)), Span::raw("  → Clear all events")]),
            Line::from(""),
            Line::from(Span::styled(" Esc/any  → cancel", Style::default().fg(Color::DarkGray))),
        ];
        let menu_height = menu_lines.len() as u16 + 2;
        let menu_width = 28;
        let area = f.area();
        let x = (area.width.saturating_sub(menu_width)) / 2;
        let y = (area.height.saturating_sub(menu_height)) / 2;
        let popup_area = Rect::new(x, y, menu_width, menu_height);
        f.render_widget(Clear, popup_area);
        let popup = Paragraph::new(menu_lines)
            .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Yellow)));
        f.render_widget(popup, popup_area);
    }
}

fn copy_to_clipboard(text: &str) {
    use std::process::{Command, Stdio};
    use std::io::Write;
    // macOS: pbcopy, Linux: xclip or wl-copy
    let mut child = if cfg!(target_os = "macos") {
        Command::new("pbcopy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    } else {
        Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .or_else(|_| Command::new("wl-copy")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn())
    };
    if let Ok(ref mut child) = child {
        if let Some(ref mut stdin) = child.stdin {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}
