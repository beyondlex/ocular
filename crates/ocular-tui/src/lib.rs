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

struct App {
    events: Vec<ProxyEvent>,
    selected: usize,
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
        components,
    };

    loop {
        while let Ok(ev) = rx.try_recv() {
            app.events.push(ev);
            // 自动跟踪最新事件
            app.selected = app.events.len().saturating_sub(1);
        }

        terminal.draw(|f| ui(f, &app))?;

        if event::poll(Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press { continue; }
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.selected = app.selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if app.selected + 1 < app.events.len() {
                            app.selected += 1;
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
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(chunks[1]);

    // 事件流
    let visible_start = app.selected.saturating_sub(right[0].height as usize);
    let event_items: Vec<ListItem> = app.events.iter().enumerate()
        .skip(visible_start)
        .map(|(i, ev)| {
            let arrow = match ev.direction {
                ocular_protocol::Direction::Request => "→",
                ocular_protocol::Direction::Response => "←",
            };
            let style = if i == app.selected {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else {
                Style::default()
            };
            ListItem::new(format!(" {} [{}] {}", arrow, ev.component, ev.summary)).style(style)
        }).collect();
    let event_list = List::new(event_items)
        .block(Block::default().borders(Borders::ALL).title(" Events (j/k navigate, q quit) "));
    f.render_widget(event_list, right[0]);

    // 详情面板
    let detail = if let Some(ev) = app.events.get(app.selected) {
        format!("Component: {}\nDirection: {:?}\nCommand:   {}\n\nRaw bytes ({} B):\n{}",
            ev.component, ev.direction, ev.summary, ev.raw.len(),
            String::from_utf8_lossy(&ev.raw))
    } else {
        "No events yet. Waiting for traffic...".to_string()
    };
    let detail_widget = Paragraph::new(detail)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title(" Detail "));
    f.render_widget(detail_widget, right[1]);
}
