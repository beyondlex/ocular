use ratatui::prelude::*;
use serde::Deserialize;

/// A single style entry from TOML
#[derive(Debug, Clone, Deserialize, Default)]
pub struct StyleConfig {
    pub fg: Option<String>,
    pub bg: Option<String>,
    #[serde(default)]
    pub bold: bool,
}

/// Theme TOML structure
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ThemeConfig {
    #[serde(default)]
    pub timestamp: StyleConfig,
    #[serde(default)]
    pub component_redis: StyleConfig,
    #[serde(default)]
    pub component_mysql: StyleConfig,
    #[serde(default)]
    pub component_rabbitmq: StyleConfig,
    #[serde(default)]
    pub component_postgres: StyleConfig,
    #[serde(default)]
    pub component_default: StyleConfig,
    #[serde(default)]
    pub command: StyleConfig,
    #[serde(default)]
    pub latency: StyleConfig,
    #[serde(default)]
    pub line_number: StyleConfig,
    #[serde(default)]
    pub selected: StyleConfig,
    #[serde(default)]
    pub visual: StyleConfig,
    #[serde(default)]
    pub border_focused: StyleConfig,
    #[serde(default)]
    pub border_normal: StyleConfig,
}

/// Runtime theme with resolved ratatui Styles
#[derive(Clone)]
pub struct Theme {
    pub timestamp: Style,
    pub component_redis: Style,
    pub component_mysql: Style,
    pub component_rabbitmq: Style,
    pub component_postgres: Style,
    pub component_default: Style,
    pub command: Style,
    pub latency: Style,
    pub line_number: Style,
    pub selected: Style,
    pub visual: Style,
    pub border_focused: Style,
    pub border_normal: Style,
}

impl Theme {
    pub fn component_style(&self, name: &str) -> Style {
        let n = name.to_lowercase();
        if n.contains("redis") {
            self.component_redis
        } else if n.contains("mysql") {
            self.component_mysql
        } else if n.contains("rabbitmq") || n.contains("amqp") {
            self.component_rabbitmq
        } else if n.contains("postgres") {
            self.component_postgres
        } else {
            self.component_default
        }
    }

    /// Load a named built-in theme, or default
    pub fn by_name(name: &str) -> Self {
        match name {
            "tokyo-night-storm" => Self::tokyo_night_storm(),
            "dracula" => Self::dracula(),
            "solarized-light" => Self::solarized_light(),
            "solarized-dark" => Self::solarized_dark(),
            _ => Self::default(),
        }
    }

    /// Build theme from TOML config, falling back to a base theme
    pub fn from_config(cfg: &ThemeConfig, base: &Theme) -> Self {
        Self {
            timestamp: apply_style_config(&cfg.timestamp, base.timestamp),
            component_redis: apply_style_config(&cfg.component_redis, base.component_redis),
            component_mysql: apply_style_config(&cfg.component_mysql, base.component_mysql),
            component_rabbitmq: apply_style_config(&cfg.component_rabbitmq, base.component_rabbitmq),
            component_postgres: apply_style_config(&cfg.component_postgres, base.component_postgres),
            component_default: apply_style_config(&cfg.component_default, base.component_default),
            command: apply_style_config(&cfg.command, base.command),
            latency: apply_style_config(&cfg.latency, base.latency),
            line_number: apply_style_config(&cfg.line_number, base.line_number),
            selected: apply_style_config(&cfg.selected, base.selected),
            visual: apply_style_config(&cfg.visual, base.visual),
            border_focused: apply_style_config(&cfg.border_focused, base.border_focused),
            border_normal: apply_style_config(&cfg.border_normal, base.border_normal),
        }
    }

    fn tokyo_night_storm() -> Self {
        Self {
            timestamp: style(Some("rgb(120,130,150)"), None, false),
            component_redis: style(Some("rgb(255,100,100)"), None, true),
            component_mysql: style(Some("rgb(125,174,255)"), None, true),
            component_rabbitmq: style(Some("rgb(255,158,100)"), None, true),
            component_postgres: style(Some("rgb(100,180,220)"), None, true),
            component_default: style(Some("rgb(115,218,202)"), None, true),
            command: style(Some("rgb(192,202,220)"), None, false),
            latency: style(Some("rgb(120,130,150)"), None, false),
            line_number: style(Some("rgb(70,80,100)"), None, false),
            selected: style(Some("rgb(220,220,240)"), Some("rgb(55,60,80)"), false),
            visual: style(Some("rgb(200,200,220)"), Some("rgb(45,50,70)"), false),
            border_focused: style(Some("rgb(125,174,255)"), None, false),
            border_normal: style(Some("rgb(60,70,90)"), None, false),
        }
    }

    fn dracula() -> Self {
        Self {
            timestamp: style(Some("rgb(98,114,164)"), None, false),
            component_redis: style(Some("rgb(255,85,85)"), None, true),
            component_mysql: style(Some("rgb(139,233,253)"), None, true),
            component_rabbitmq: style(Some("rgb(255,184,108)"), None, true),
            component_postgres: style(Some("rgb(139,233,253)"), None, true),
            component_default: style(Some("rgb(80,250,123)"), None, true),
            command: style(Some("rgb(248,248,242)"), None, false),
            latency: style(Some("rgb(98,114,164)"), None, false),
            line_number: style(Some("rgb(68,71,90)"), None, false),
            selected: style(Some("rgb(248,248,242)"), Some("rgb(68,71,90)"), false),
            visual: style(Some("rgb(248,248,242)"), Some("rgb(55,58,75)"), false),
            border_focused: style(Some("rgb(189,147,249)"), None, false),
            border_normal: style(Some("rgb(68,71,90)"), None, false),
        }
    }

    fn solarized_light() -> Self {
        Self {
            timestamp: style(Some("rgb(147,161,161)"), None, false),
            component_redis: style(Some("rgb(220,50,47)"), None, true),
            component_mysql: style(Some("rgb(38,139,210)"), None, true),
            component_rabbitmq: style(Some("rgb(203,75,22)"), None, true),
            component_postgres: style(Some("rgb(42,161,152)"), None, true),
            component_default: style(Some("rgb(133,153,0)"), None, true),
            command: style(Some("rgb(88,110,117)"), None, false),
            latency: style(Some("rgb(147,161,161)"), None, false),
            line_number: style(Some("rgb(188,192,180)"), None, false),
            selected: style(Some("rgb(7,54,66)"), Some("rgb(238,232,213)"), false),
            visual: style(Some("rgb(7,54,66)"), Some("rgb(227,224,205)"), false),
            border_focused: style(Some("rgb(38,139,210)"), None, false),
            border_normal: style(Some("rgb(188,192,180)"), None, false),
        }
    }

    fn solarized_dark() -> Self {
        Self {
            timestamp: style(Some("rgb(88,110,117)"), None, false),
            component_redis: style(Some("rgb(220,50,47)"), None, true),
            component_mysql: style(Some("rgb(38,139,210)"), None, true),
            component_rabbitmq: style(Some("rgb(203,75,22)"), None, true),
            component_postgres: style(Some("rgb(42,161,152)"), None, true),
            component_default: style(Some("rgb(133,153,0)"), None, true),
            command: style(Some("rgb(147,161,161)"), None, false),
            latency: style(Some("rgb(88,110,117)"), None, false),
            line_number: style(Some("rgb(0,43,54)"), None, false),
            selected: style(Some("rgb(253,246,227)"), Some("rgb(7,54,66)"), false),
            visual: style(Some("rgb(238,232,213)"), Some("rgb(0,43,54)"), false),
            border_focused: style(Some("rgb(38,139,210)"), None, false),
            border_normal: style(Some("rgb(0,43,54)"), None, false),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            timestamp: style(Some("rgb(140,140,140)"), None, false),
            component_redis: style(Some("rgb(255,80,80)"), None, true),
            component_mysql: style(Some("rgb(80,140,255)"), None, true),
            component_rabbitmq: style(Some("rgb(255,140,0)"), None, true),
            component_postgres: style(Some("rgb(100,180,220)"), None, true),
            component_default: style(Some("rgb(80,200,120)"), None, true),
            command: style(Some("rgb(220,220,220)"), None, false),
            latency: style(Some("rgb(140,140,140)"), None, false),
            line_number: style(Some("rgb(100,100,100)"), None, false),
            selected: style(Some("rgb(255,255,255)"), Some("rgb(50,50,70)"), false),
            visual: style(Some("rgb(255,255,255)"), Some("rgb(60,60,80)"), false),
            border_focused: style(Some("cyan"), None, false),
            border_normal: style(None, None, false),
        }
    }
}

fn style(fg: Option<&str>, bg: Option<&str>, bold: bool) -> Style {
    let mut s = Style::default();
    if let Some(c) = fg.and_then(parse_color) { s = s.fg(c); }
    if let Some(c) = bg.and_then(parse_color) { s = s.bg(c); }
    if bold { s = s.add_modifier(Modifier::BOLD); }
    s
}

fn apply_style_config(cfg: &StyleConfig, base: Style) -> Style {
    if cfg.fg.is_none() && cfg.bg.is_none() && !cfg.bold {
        return base;
    }
    let mut s = base;
    if let Some(c) = cfg.fg.as_deref().and_then(parse_color) { s = s.fg(c); }
    if let Some(c) = cfg.bg.as_deref().and_then(parse_color) { s = s.bg(c); }
    if cfg.bold { s = s.add_modifier(Modifier::BOLD); }
    s
}

fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some(Color::Rgb(r, g, b));
        }
    }
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 3 {
            let r = parts[0].trim().parse().ok()?;
            let g = parts[1].trim().parse().ok()?;
            let b = parts[2].trim().parse().ok()?;
            return Some(Color::Rgb(r, g, b));
        }
    }
    match s.to_lowercase().as_str() {
        "black" => Some(Color::Black),
        "red" => Some(Color::Red),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "blue" => Some(Color::Blue),
        "magenta" => Some(Color::Magenta),
        "cyan" => Some(Color::Cyan),
        "white" => Some(Color::White),
        "gray" | "grey" => Some(Color::Gray),
        "darkgray" => Some(Color::DarkGray),
        _ => None,
    }
}
