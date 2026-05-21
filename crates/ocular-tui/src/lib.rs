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

#[derive(PartialEq)]
enum Focus { Events, Detail }

struct App {
    events: Vec<ProxyEvent>,
    selected: usize,
    detail_scroll: u16,
    focus: Focus,
    components: Vec<ComponentInfo>,
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
    };

    loop {
        while let Ok(ev) = rx.try_recv() {
            app.events.push(ev);
            if app.focus == Focus::Events {
                app.selected = app.events.len().saturating_sub(1);
            }
        }

        terminal.draw(|f| ui(f, &app))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press { continue; }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Tab => {
                        app.focus = match app.focus {
                            Focus::Events => Focus::Detail,
                            Focus::Detail => Focus::Events,
                        };
                        app.detail_scroll = 0;
                    }
                    KeyCode::Up | KeyCode::Char('k') => match app.focus {
                        Focus::Events => { app.selected = app.selected.saturating_sub(1); app.detail_scroll = 0; }
                        Focus::Detail => { app.detail_scroll = app.detail_scroll.saturating_sub(1); }
                    },
                    KeyCode::Down | KeyCode::Char('j') => match app.focus {
                        Focus::Events => {
                            if app.selected + 1 < app.events.len() {
                                app.selected += 1;
                                app.detail_scroll = 0;
                            }
                        }
                        Focus::Detail => { app.detail_scroll += 1; }
                    },
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

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints([Constraint::Length(28), Constraint::Min(0)])
        .split(f.area());

    // 左侧：组件列表
    let items: Vec<ListItem> = app.components.iter().map(|c| {
        ListItem::new(format!(" 🟢 {} ({})", c.name, c.listen))
    }).collect();
    let left = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Components "));
    f.render_widget(left, chunks[0]);

    // 右侧上下分割
    let right = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[1]);

    // 事件流
    let events_focused = app.focus == Focus::Events;
    let visible_start = app.selected.saturating_sub(right[0].height.saturating_sub(2) as usize);
    let event_items: Vec<ListItem> = app.events.iter().enumerate()
        .skip(visible_start)
        .map(|(i, ev)| {
            let arrow = match ev.direction {
                ocular_protocol::Direction::Request => "→",
                ocular_protocol::Direction::Response => "←",
            };
            let time = format_time(&ev.timestamp);
            let lat = ev.latency.as_ref().map(|d| format!(" ({})", format_latency(d))).unwrap_or_default();
            let line = format!(" {} {} [{}] {}{}", time, arrow, ev.component, ev.summary, lat);
            let style = if i == app.selected {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };
            ListItem::new(line).style(style)
        }).collect();
    let events_border_style = if events_focused { Style::default().fg(Color::Cyan) } else { Style::default() };
    let event_list = List::new(event_items)
        .block(Block::default().borders(Borders::ALL).border_style(events_border_style)
            .title(" Events (Tab switch, j/k navigate, q quit) "));
    f.render_widget(event_list, right[0]);

    // 详情面板
    let detail_focused = app.focus == Focus::Detail;
    let detail = if let Some(ev) = app.events.get(app.selected) {
        let lat_str = ev.latency.as_ref().map(|d| format_latency(d)).unwrap_or_else(|| "-".into());
        let raw_display = String::from_utf8_lossy(&ev.raw)
            .replace("\r\n", "\\r\\n\n");
        format!("Time:      {}\nComponent: {}\nDirection: {:?}\nLatency:   {}\nCommand:   {}\n\nRaw ({} bytes):\n{}",
            format_time(&ev.timestamp), ev.component, ev.direction, lat_str,
            ev.summary, ev.raw.len(), raw_display)
    } else {
        "No events yet. Waiting for traffic...".to_string()
    };
    let detail_border_style = if detail_focused { Style::default().fg(Color::Cyan) } else { Style::default() };
    let title = if detail_focused { " Detail (j/k scroll) " } else { " Detail (Tab to focus) " };
    let detail_widget = Paragraph::new(detail)
        .wrap(Wrap { trim: false })
        .scroll((app.detail_scroll, 0))
        .block(Block::default().borders(Borders::ALL).border_style(detail_border_style).title(title));
    f.render_widget(detail_widget, right[1]);
}
