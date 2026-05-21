use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ocular_proxy::ProxyEvent;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
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
    };

    loop {
        while let Ok(ev) = rx.try_recv() {
            app.events.push(ev);
            if app.focus == Focus::Events && app.filter.is_empty() {
                let filtered = app.filtered_events();
                app.selected = filtered.len().saturating_sub(1);
            }
        }

        terminal.draw(|f| ui(f, &app))?;

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

                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char('/') => {
                        app.focus = Focus::Filter;
                    }
                    KeyCode::Esc => {
                        if !app.filter.is_empty() {
                            app.filter.clear();
                        } else {
                            app.component_idx = None;
                        }
                        app.selected = 0;
                        app.focus = Focus::Events;
                    }
                    KeyCode::Tab => {
                        app.focus = match app.focus {
                            Focus::Components => Focus::Events,
                            Focus::Events => Focus::Detail,
                            Focus::Detail => Focus::Components,
                            Focus::Filter => Focus::Events,
                        };
                        app.detail_scroll = 0;
                    }
                    KeyCode::BackTab => {
                        app.focus = match app.focus {
                            Focus::Components => Focus::Detail,
                            Focus::Events => Focus::Components,
                            Focus::Detail => Focus::Events,
                            Focus::Filter => Focus::Events,
                        };
                        app.detail_scroll = 0;
                    }
                    KeyCode::Up | KeyCode::Char('k') => match app.focus {
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
                    },
                    KeyCode::Down | KeyCode::Char('j') => match app.focus {
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
                    },
                    KeyCode::Enter => {
                        if app.focus == Focus::Components {
                            app.focus = Focus::Events;
                            app.selected = 0;
                        }
                    }
                    _ => {}
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

fn ui(f: &mut Frame, app: &App) {
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
            let line = format!(" {} {} [{}] {}{}", time, arrow, ev.component, ev.summary, lat);
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
}
