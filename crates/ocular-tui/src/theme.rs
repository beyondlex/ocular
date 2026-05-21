use ratatui::prelude::*;

/// Theme configuration for UI elements
#[derive(Clone)]
pub struct Theme {
    pub timestamp: Style,
    pub component_redis: Style,
    pub component_mysql: Style,
    pub component_default: Style,
    pub command: Style,
    pub latency: Style,
    pub line_number: Style,
    pub selected: Style,
    pub visual: Style,
    pub border_focused: Style,
    pub border_normal: Style,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            timestamp: Style::default().fg(Color::Rgb(140, 140, 140)),
            component_redis: Style::default().fg(Color::Rgb(255, 140, 0)).add_modifier(Modifier::BOLD),
            component_mysql: Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
            component_default: Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            command: Style::default().fg(Color::White),
            latency: Style::default().fg(Color::Rgb(140, 140, 140)),
            line_number: Style::default().fg(Color::Rgb(100, 100, 100)),
            selected: Style::default().bg(Color::Rgb(50, 50, 70)).fg(Color::White),
            visual: Style::default().bg(Color::Rgb(60, 60, 80)).fg(Color::White),
            border_focused: Style::default().fg(Color::Cyan),
            border_normal: Style::default(),
        }
    }
}

impl Theme {
    pub fn component_style(&self, name: &str) -> Style {
        match name {
            "redis" => self.component_redis,
            "mysql" => self.component_mysql,
            _ => self.component_default,
        }
    }
}
