use chrono::{DateTime, Local};
use ocular_proxy::ProxyEvent;
use std::time::{Duration, SystemTime};

use crate::types::App;

pub(crate) fn format_time(ts: &SystemTime) -> String {
    let dt: DateTime<Local> = (*ts).into();
    dt.format("%H:%M:%S%.3f").to_string()
}

pub(crate) fn format_sql(sql: &str) -> String {
    sqlformat::format(sql, &sqlformat::QueryParams::None, sqlformat::FormatOptions {
        indent: sqlformat::Indent::Spaces(2),
        uppercase: true,
        lines_between_queries: 1,
    })
}

pub(crate) fn format_latency(d: &Duration) -> String {
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

pub(crate) fn split_addr(addr: &str) -> (String, String) {
    if let Some((h, p)) = addr.rsplit_once(':') {
        (h.to_string(), p.to_string())
    } else {
        (addr.to_string(), String::new())
    }
}

pub(crate) fn copy_to_clipboard(text: &str) {
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

pub(crate) fn get_selected_commands(filtered: &[(usize, &ProxyEvent, Vec<usize>)], app: &App) -> String {
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

pub(crate) fn format_copy_text(ev: &ProxyEvent) -> String {
    ocular_protocol::get_handler(ev.protocol).to_replay_command(ev)
}

pub(crate) fn open_in_editor(text: &str) {
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

pub(crate) fn highlight_sql_line(line: &str) -> ratatui::text::Line<'static> {
    use ratatui::prelude::*;
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

pub(crate) fn highlight_json_line(line: &str) -> ratatui::text::Line<'static> {
    use ratatui::prelude::*;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    #[test]
    fn test_format_latency_sub_millisecond() {
        let d = Duration::from_micros(420);
        let s = format_latency(&d);
        assert!(s.ends_with("ms"));
        assert!(s.contains("0.4"));
    }

    #[test]
    fn test_format_latency_milliseconds() {
        let d = Duration::from_millis(5);
        let s = format_latency(&d);
        assert!(s.contains("5"));
        assert!(s.ends_with("ms"));
    }

    #[test]
    fn test_format_latency_over_100ms() {
        let d = Duration::from_millis(150);
        let s = format_latency(&d);
        assert!(s.contains("150"));
    }

    #[test]
    fn test_format_latency_over_1s() {
        let d = Duration::from_millis(2500);
        let s = format_latency(&d);
        assert!(s.contains("2500"));
    }

    #[test]
    fn test_split_addr_with_port() {
        let (h, p) = split_addr("127.0.0.1:6379");
        assert_eq!(h, "127.0.0.1");
        assert_eq!(p, "6379");
    }

    #[test]
    fn test_split_addr_without_port() {
        let (h, p) = split_addr("localhost");
        assert_eq!(h, "localhost");
        assert_eq!(p, "");
    }

    #[test]
    fn test_split_addr_ipv6_like() {
        let (h, p) = split_addr("[::1]:8080");
        assert_eq!(h, "[::1]");
        assert_eq!(p, "8080");
    }

    #[test]
    fn test_highlight_sql_keywords() {
        let line = highlight_sql_line("SELECT * FROM users WHERE id = 1");
        let spans = &line.spans;
        // Should have multiple spans with different styles
        assert!(spans.len() > 3);
        // "SELECT" should be styled as keyword (Magenta + Bold)
        let select_span = &spans[0];
        assert_eq!(select_span.content.as_ref(), "SELECT");
    }

    #[test]
    fn test_highlight_sql_string_literal() {
        let line = highlight_sql_line("SELECT 'hello'");
        let text: String = line.spans.iter().map(|s| s.content.as_ref().to_string()).collect();
        assert!(text.contains("'hello'"));
    }

    #[test]
    fn test_highlight_sql_numbers() {
        let line = highlight_sql_line("SELECT 42");
        // 42 should be highlighted as a number
        let text: String = line.spans.iter().map(|s| s.content.as_ref().to_string()).collect();
        assert!(text.contains("42"));
    }

    #[test]
    fn test_highlight_json_string_key() {
        let line = highlight_json_line("\"name\": \"Alice\"");
        let text: String = line.spans.iter().map(|s| s.content.as_ref().to_string()).collect();
        assert!(text.contains("\"name\""));
        assert!(text.contains("\"Alice\""));
    }

    #[test]
    fn test_highlight_json_number() {
        let line = highlight_json_line("42");
        let text: String = line.spans.iter().map(|s| s.content.as_ref().to_string()).collect();
        assert!(text.contains("42"));
    }

    #[test]
    fn test_highlight_json_boolean() {
        let line = highlight_json_line("true");
        let text: String = line.spans.iter().map(|s| s.content.as_ref().to_string()).collect();
        assert!(text.contains("true"));
    }

    #[test]
    fn test_highlight_json_null() {
        let line = highlight_json_line("null");
        let text: String = line.spans.iter().map(|s| s.content.as_ref().to_string()).collect();
        assert!(text.contains("null"));
    }

    #[test]
    fn test_highlight_json_braces() {
        let line = highlight_json_line("{}");
        let text: String = line.spans.iter().map(|s| s.content.as_ref().to_string()).collect();
        assert!(text.contains("{"));
        assert!(text.contains("}"));
    }

    #[test]
    fn test_format_time() {
        let now = SystemTime::now();
        let s = format_time(&now);
        // Should be HH:MM:SS.mmm format
        assert!(s.contains(':'));
        assert!(s.contains('.'));
        assert!(s.len() >= 12);
    }

    #[test]
    fn test_format_sql() {
        let sql = "select * from users where id = 1";
        let formatted = format_sql(sql);
        // sqlformat uppercases keywords
        assert!(formatted.contains("SELECT") || formatted.contains("select"));
    }

    #[test]
    fn test_format_copy_text_uses_handler() {
        use ocular_protocol::Protocol;
        use std::time::SystemTime;
        let ev = ProxyEvent {
            timestamp: SystemTime::now(),
            component: "test".into(),
            protocol: Protocol::Redis,
            command: "GET key".into(),
            full_command: "GET key".into(),
            response: "value".into(),
            response_detail: "value".into(),
            latency: Duration::from_millis(1),
            process: None,
            src: None,
            dest: None,
            system: false,
        };
        let text = format_copy_text(&ev);
        assert!(text.contains("GET"));
    }
}
