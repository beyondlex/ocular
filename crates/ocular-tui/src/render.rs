use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
};
use std::time::Duration;

use crate::helpers::{format_time, format_latency, format_sql, highlight_sql_line, highlight_json_line};
use crate::types::*;

pub(crate) fn ui_dashboard(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3)])
        .split(area);

    let main_area = chunks[0];
    let box_w: u16 = 52;

    match &app.mode {
        AppMode::Dashboard => {
            let mut lines: Vec<Line> = Vec::new();

            let visible = app.dashboard.filtered_indices();
            let max_visible: usize = 10;
            let selected_pos = visible.iter().position(|&i| i == app.dashboard.selected).unwrap_or(0);
            let scroll_start = if selected_pos < max_visible {
                0
            } else {
                selected_pos - max_visible + 1
            };
            let visible_window = &visible[scroll_start..visible.len().min(scroll_start + max_visible)];

            for &i in visible_window {
                let g = &app.dashboard.groups[i];
                let is_selected = i == app.dashboard.selected;
                let prefix = if is_selected { " ● " } else { "   " };
                let proxies_str = if g.proxies.is_empty() {
                    "(empty)".to_string()
                } else {
                    let s = g.proxies.join(", ");
                    if s.len() > 28 { format!("{}...", &s[..25]) } else { s }
                };
                let name_style = if is_selected {
                    Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                lines.push(Line::from(vec![
                    Span::styled(prefix, if is_selected { Style::default().fg(Color::Cyan) } else { Style::default() }),
                    Span::styled(format!("{:<10}", g.name), name_style),
                    Span::styled(format!(" [{}]", proxies_str), Style::default().fg(Color::DarkGray)),
                ]));
            }
            // Scroll indicator
            if visible.len() > max_visible {
                let indicator = format!(" ({}/{})", selected_pos + 1, visible.len());
                lines.push(Line::from(Span::styled(indicator, Style::default().fg(Color::DarkGray))));
            }

            if !app.dashboard.filter.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(" (filter: {})", app.dashboard.filter),
                    Style::default().fg(Color::Rgb(255, 165, 0)),
                )));
            }
            lines.push(Line::from(""));

            let box_h = (lines.len() as u16 + 2).min(main_area.height.saturating_sub(6));
            let art_h: u16 = 4; // 3 lines ASCII art + 1 line version
            let gap: u16 = 1;
            let x = (main_area.width.saturating_sub(box_w)) / 2;
            let y = (main_area.height.saturating_sub(art_h + gap + box_h)) / 2 + art_h + gap;
            let box_area = Rect::new(x, y, box_w, box_h);
            let block = Block::default().borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(Color::DarkGray))
                .title_top(Line::from(vec![Span::styled(" Groups ", Style::default().fg(Color::DarkGray))]));
            let content = Paragraph::new(lines).block(block);
            f.render_widget(content, box_area);

            // Render ASCII art title above box
            let ascii_art = [
                "▄▖    ▜     ",
                "▌▌▛▘▌▌▐ ▀▌▛▘",
                "▙▌▙▖▙▌▐▖█▌▌ ",
            ];

            let mut art_lines: Vec<Line> = ascii_art.iter().map(|s| {
                Line::from(Span::styled(*s, Style::default().fg(Color::Cyan))).centered()
            }).collect();
            art_lines.push(
                Line::from(Span::styled(
                    format!("v{}", env!("CARGO_PKG_VERSION")),
                    Style::default().fg(Color::DarkGray),
                )).centered(),
            );
            let art_area = Rect::new(x, y - gap - art_h, box_w, art_h);
            f.render_widget(Paragraph::new(art_lines), art_area);

            // Filter input at bottom of box
            if app.dashboard.filter_active {
                let filter_area = Rect::new(x, y + box_h, box_w, 1);
                let filter_line = Paragraph::new(Line::from(Span::styled(
                    format!(" /{}", app.dashboard.filter),
                    Style::default().fg(Color::Rgb(255, 165, 0)),
                )));
                f.render_widget(filter_line, filter_area);
            }

            // Status bar
            let key_style = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
            let sep = Style::default().fg(Color::DarkGray);
            let status = if app.dashboard.filter_active {
                Line::from(Span::styled(format!(" /{}", app.dashboard.filter), Style::default().fg(Color::Rgb(255, 165, 0))))
            } else {
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled("n", key_style), Span::raw(" new "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("r", key_style), Span::raw(" rename "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("e", key_style), Span::raw(" edit "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("d", key_style), Span::raw(" delete "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("/", key_style), Span::raw(" filter "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("Space", key_style), Span::raw(" detail "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("↵", key_style), Span::raw(" load "),
                    Span::styled("│", sep), Span::raw(" "),
                    Span::styled("q", key_style), Span::raw(" quit"),
                ]).centered()
            };
            let status_block = Block::default().borders(Borders::TOP | Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray));
            let status_inner = status_block.inner(chunks[1]);
            f.render_widget(status_block, chunks[1]);
            f.render_widget(Paragraph::new(status), status_inner);
            // Delete confirm popup
            if app.dashboard.delete_confirm {
                if let Some(g) = app.dashboard.groups.get(app.dashboard.selected) {
                    let w: u16 = 36;
                    let h: u16 = 5;
                    let x = (area.width.saturating_sub(w)) / 2;
                    let y = (area.height.saturating_sub(h)) / 2;
                    let popup_area = Rect::new(x, y, w, h);
                    f.render_widget(Clear, popup_area);
                    let lines = vec![
                        Line::from(format!(" Delete \"{}\"?", g.name)),
                        Line::from(""),
                        Line::from(vec![
                            Span::raw(" "),
                            Span::styled("y/Enter", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                            Span::raw(" confirm  "),
                            Span::styled("n/Esc", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                            Span::raw(" cancel"),
                        ]),
                    ];
                    let popup = Paragraph::new(lines).block(Block::default().borders(Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded)
                        .border_style(Style::default().fg(Color::Yellow))
                        .title(" Confirm "));
                    f.render_widget(popup, popup_area);
                }
            }
        }
        AppMode::NewGroupName | AppMode::RenameGroup => {
            let is_rename = app.mode == AppMode::RenameGroup;
            let title = if is_rename { " Rename Group " } else { " New Group " };
            let input = if is_rename { &app.dashboard.rename_input } else { &app.dashboard.new_group_name };

            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(" Group name: ", Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{}▌", input), Style::default().fg(Color::White)),
            ]));
            if let Some(ref err) = app.dashboard.error {
                lines.push(Line::from(Span::styled(format!(" ⚠ {}", err), Style::default().fg(Color::Red))));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(" Enter: confirm  Esc: cancel", Style::default().fg(Color::DarkGray))));

            let box_h = lines.len() as u16 + 2;
            let x = (main_area.width.saturating_sub(box_w)) / 2;
            let y = (main_area.height.saturating_sub(box_h)) / 2;
            let box_area = Rect::new(x, y, box_w, box_h);
            let block = Block::default().borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title);
            f.render_widget(Paragraph::new(lines).block(block), box_area);

            if !is_rename && y + box_h < main_area.height {
                let hint_area = Rect::new(x, y + box_h + 1, box_w, 1);
                let hint = Paragraph::new(Line::from(Span::styled(
                    "Create a group for organizing proxies",
                    Style::default().fg(Color::Rgb(80, 80, 80)),
                ))).centered();
                f.render_widget(hint, hint_area);
            }
        }
        AppMode::GroupDetail => {
            let mut lines: Vec<Line> = Vec::new();
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(" Group: {}", app.dashboard.detail_group_name),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));

            if app.dashboard.detail_proxies.is_empty() {
                lines.push(Line::from(Span::styled(" No proxies", Style::default().fg(Color::DarkGray))));
            } else {
                // Scrolling: calculate visible window based on available height
                // Reserve: 3 header lines + 2 footer lines + 2 borders = 7
                let max_items = (main_area.height as usize).saturating_sub(9);
                let total = app.dashboard.detail_proxies.len();
                let selected = app.dashboard.detail_selected;
                let scroll_start = if selected < max_items {
                    0
                } else {
                    selected - max_items + 1
                };
                let visible_end = (scroll_start + max_items).min(total);
                for i in scroll_start..visible_end {
                    let p = &app.dashboard.detail_proxies[i];
                    let is_selected = i == selected;
                    let prefix = if is_selected { " ▸ " } else { "   " };
                    let name_style = if is_selected {
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    };
                    lines.push(Line::from(vec![
                        Span::styled(prefix, Style::default().fg(Color::Cyan)),
                        Span::styled(if p.mode == "capture" { "⊙" } else { "⇄" }, Style::default().fg(Color::DarkGray)),
                        Span::styled(format!(" {:<10}", p.name), name_style),
                        Span::styled(format!(" {} \u{2192} {} ", p.listen, p.remote), Style::default().fg(Color::DarkGray)),
                    ]));
                }
                if visible_end < total {
                    lines.push(Line::from(Span::styled(
                        format!(" ({}/{})", selected + 1, total),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
            }
            lines.push(Line::from(""));
            if app.proxy_form.is_none() {
                lines.push(Line::from(Span::styled(
                    " n add  |  e edit  |  d delete  |  Esc back",
                    Style::default().fg(Color::DarkGray),
                )));
            }

            let box_h = (lines.len() as u16 + 2).min(main_area.height.saturating_sub(2));
            let x = (main_area.width.saturating_sub(box_w)) / 2;
            let y = (main_area.height.saturating_sub(box_h)) / 2;
            let box_area = Rect::new(x, y, box_w, box_h);
            let block = Block::default().borders(Borders::ALL)
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(Color::Cyan))
                .title_top(Line::from(vec![
                    Span::styled(" ", Style::default()),
                    Span::styled(app.dashboard.detail_group_name.clone(), Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                    Span::styled(" ", Style::default()),
                ]));
            let content = Paragraph::new(lines).block(block);
            f.render_widget(content, box_area);

            // Proxy form popup
            if let Some(ref form) = app.proxy_form {
                let fw: u16 = 42;
                let fh: u16 = 13;
                let fx = (area.width.saturating_sub(fw)) / 2;
                let fy = (area.height.saturating_sub(fh)) / 2;
                let popup_area = Rect::new(fx, fy, fw, fh);
                f.render_widget(Clear, popup_area);

                let protocol = PROTOCOLS[form.protocol_idx];
                let mode = MODES[form.mode_idx];
                let remote_default_port = default_port(protocol);
                let mut rows: Vec<(usize, &str, &str, &str)> = vec![
                    (0, "name", form.inputs[0].value(), ""),
                    (1, "protocol", protocol, ""),
                    (2, "mode", mode, ""),
                ];
                if form.mode_idx == 1 {
                    rows.push((3, "remote host", form.inputs[3].value(), "127.0.0.1"));
                    rows.push((4, "remote port", form.inputs[4].value(), remote_default_port));
                    rows.push((5, "interface", form.inputs[5].value(), DEFAULT_IFACE));
                } else if form.editing_idx.is_some() {
                    rows.push((3, "listen host", form.inputs[1].value(), "127.0.0.1"));
                    rows.push((4, "listen port", form.inputs[2].value(), ""));
                    rows.push((5, "remote host", form.inputs[3].value(), "127.0.0.1"));
                    rows.push((6, "remote port", form.inputs[4].value(), remote_default_port));
                } else {
                    rows.push((3, "remote host", form.inputs[3].value(), "127.0.0.1"));
                    rows.push((4, "remote port", form.inputs[4].value(), remote_default_port));
                }
                let max_val_w = (fw as usize).saturating_sub(2 + 18 + 1);
                let mut form_lines: Vec<Line> = Vec::new();
                for &(i, label, value, placeholder) in &rows {
                    let is_active = i == form.active_field;
                    let label_style = if is_active { Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::DarkGray) };
                    let hint = if (i == 1 || i == 2) && is_active { " ◀ ▶" } else { "" };
                    let display = if value.is_empty() && !placeholder.is_empty() && !is_active {
                        vec![Span::styled(format!("   {:>12}: ", label), label_style), Span::styled(placeholder.to_string(), Style::default().fg(Color::Rgb(80, 80, 80)))]
                    } else if value.is_empty() && !placeholder.is_empty() && is_active {
                        vec![Span::styled(format!("   {:>12}: ", label), label_style), Span::styled("▌", Style::default().fg(Color::White)), Span::styled(format!(" ({})", placeholder), Style::default().fg(Color::Rgb(80, 80, 80)))]
                    } else if is_active {
                        let cur = form.field_idx().map(|fi| form.inputs[fi].cursor().min(value.len())).unwrap_or(value.len());
                        let (vs, ve) = if value.len() <= max_val_w { (0, value.len()) } else if cur <= max_val_w / 2 { (0, max_val_w) } else if cur >= value.len() - max_val_w / 2 { (value.len() - max_val_w, value.len()) } else { (cur - max_val_w / 2, cur + max_val_w / 2) };
                        let prefix = if vs > 0 { "…" } else { "" };
                        let suffix = if ve < value.len() { "…" } else { "" };
                        vec![Span::styled(format!("   {:>12}: ", label), label_style), Span::styled(format!("{}{}", prefix, &value[vs..cur]), Style::default().fg(Color::White)), Span::styled("▌", Style::default().fg(Color::Cyan)), Span::styled(format!("{}{}", &value[cur..ve], suffix), Style::default().fg(Color::White)), Span::styled(hint.to_string(), Style::default().fg(Color::DarkGray))]
                    } else {
                        let vis = if value.len() > max_val_w { format!("{}…", &value[..max_val_w - 1]) } else { value.to_string() };
                        vec![Span::styled(format!("   {:>12}: ", label), label_style), Span::styled(vis, Style::default().fg(Color::White)), Span::styled(hint.to_string(), Style::default().fg(Color::DarkGray))]
                    };
                    form_lines.push(Line::from(display));
                }
                let error_line = form.error.as_ref().map(|err| Line::from(Span::styled(format!("   ⚠ {}", err), Style::default().fg(Color::Red))));
                let hint_line = Line::from(vec![
                    Span::raw("   "),
                    Span::styled("Esc", Style::default().fg(Color::DarkGray)), Span::raw(" cancel  "),
                    Span::styled("Enter", Style::default().fg(Color::Green)), Span::raw(" save"),
                ]);
                let inner_h = (fh - 2) as usize;
                let bottom_n = if error_line.is_some() { 2 } else { 1 };
                let pad = inner_h.saturating_sub(form_lines.len() + bottom_n);
                for _ in 0..pad { form_lines.push(Line::from("")); }
                if let Some(el) = error_line { form_lines.push(el); }
                form_lines.push(hint_line);
                let popup = Paragraph::new(form_lines)
                    .block(Block::default().borders(Borders::ALL)
                        .border_type(ratatui::widgets::BorderType::Rounded)
                        .border_style(Style::default().fg(Color::Cyan)).title(if form.editing_idx.is_some() { " Edit Proxy " } else { " Add Proxy " }));
                f.render_widget(popup, popup_area);
            }

            // Delete confirm popup
            if app.dashboard.detail_delete_confirm {
                let w: u16 = 36;
                let h: u16 = 5;
                let x = (area.width.saturating_sub(w)) / 2;
                let y = (area.height.saturating_sub(h)) / 2;
                let popup_area = Rect::new(x, y, w, h);
                f.render_widget(Clear, popup_area);
                let name = app.dashboard.detail_proxies.get(app.dashboard.detail_selected).map(|p| p.name.as_str()).unwrap_or("");
                let lines = vec![
                    Line::from(format!(" Delete \"{}\"?", name)),
                    Line::from(""),
                    Line::from(vec![
                        Span::raw(" "),
                        Span::styled("y/Enter", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                        Span::raw(" confirm  "),
                        Span::styled("n/Esc", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                        Span::raw(" cancel"),
                    ]),
                ];
                let popup = Paragraph::new(lines).block(Block::default().borders(Borders::ALL)
                    .border_type(ratatui::widgets::BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Yellow))
                    .title(" Confirm "));
                f.render_widget(popup, popup_area);
            }

            // Status bar
            let key_style = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
            let sep = Style::default().fg(Color::DarkGray);
            let status = Line::from(vec![
                Span::raw(" "),
                Span::styled("n", key_style), Span::raw(" add "),
                Span::styled("│", sep), Span::raw(" "),
                Span::styled("e", key_style), Span::raw(" edit "),
                Span::styled("│", sep), Span::raw(" "),
                Span::styled("d", key_style), Span::raw(" delete "),
                Span::styled("│", sep), Span::raw(" "),
                Span::styled("Esc", key_style), Span::raw(" back"),
            ]).centered();
            let status_block = Block::default().borders(Borders::TOP | Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray));
            let status_inner = status_block.inner(chunks[1]);
            f.render_widget(status_block, chunks[1]);
            f.render_widget(Paragraph::new(status), status_inner);
        }
        AppMode::Main => {}
    }
}

pub(crate) fn ui(f: &mut Frame, app: &mut App) {
    let outer = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(f.area());

    let chunks = Layout::default()
        .direction(ratatui::layout::Direction::Horizontal)
        .constraints(if app.preview && app.components.len() <= 1 {
            [Constraint::Length(0), Constraint::Min(0)]
        } else {
            [Constraint::Length(28), Constraint::Min(0)]
        })
        .split(outer[0]);

    // Left: component list
    let comp_focused = app.focus == Focus::Components || app.focus == Focus::ComponentFilter;
    let fuzzy = SkimMatcherV2::default();
    let filter_active = !app.component_filter.is_empty();

    // Build "ALL" row with group name
    let all_label: Line = if let Some(ref group) = app.active_group {
        Line::from(vec![
            Span::raw(if app.component_idx.is_none() { " > All " } else { "   All " }),
            Span::styled(format!("[{}]", group), Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(if app.component_idx.is_none() { " > All" } else { "   All" })
    };
    let all_style = if app.component_idx.is_none() && comp_focused {
        Style::default().bg(Color::DarkGray).fg(Color::White)
    } else if app.component_idx.is_none() {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let mut items: Vec<ListItem> = vec![ListItem::new(all_label).style(all_style)];

    if filter_active {
        items.push(ListItem::new(Line::from(Span::styled(
            format!(" (filter: {})", app.component_filter),
            Style::default().fg(Color::Rgb(255, 165, 0)),
        ))));
        items.extend(app.components.iter().enumerate().filter(|(_, c)| {
            let target = format!("{} {} {}", c.name, c.listen, c.listen);
            fuzzy.fuzzy_match(&target, &app.component_filter).is_some()
        }).map(|(i, c)| {
            let selected = app.component_idx == Some(i);
            let prefix = if selected { " >" } else { "  " };
            let style = if selected && comp_focused {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let status = app.status_map.lock().unwrap().get(&c.name).cloned();
            let in_cooldown = status.as_ref()
                .and_then(|s| s.last_active_at.as_ref())
                .and_then(|t| t.elapsed().ok())
                .map(|d| d < Duration::from_secs(3))
                .unwrap_or(false);
            let (dot, dot_color) = match status {
                Some(s) if s.last_error.is_some() => ("◎", Color::Yellow),
                Some(s) if s.active_connections > 0 => ("●", Color::Green),
                _ if in_cooldown => ("●", Color::Green),
                Some(s) if s.has_connector => ("○", Color::DarkGray),
                _ => ("○", Color::Gray),
            };
            let count = app.events.iter().filter(|ev| ev.component == c.name).count();
            let count_style = if count > 0 { Style::default().fg(Color::Green) } else { Style::default().fg(Color::DarkGray) };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{} ", prefix)),
                Span::styled(dot, Style::default().fg(dot_color)),
                Span::styled(if c.listen.is_empty() { " ⊙" } else { " ⇄" }, Style::default().fg(Color::DarkGray)),
                Span::styled(format!(" {}", c.name), style),
                Span::styled(format!(" {}", count), count_style),
            ])).style(style)
        }));
    } else {
        items.extend(app.components.iter().enumerate().map(|(i, c)| {
            let selected = app.component_idx == Some(i);
            let prefix = if selected { " >" } else { "  " };
            let style = if selected && comp_focused {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else if selected {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            let status = app.status_map.lock().unwrap().get(&c.name).cloned();
            let in_cooldown = status.as_ref()
                .and_then(|s| s.last_active_at.as_ref())
                .and_then(|t| t.elapsed().ok())
                .map(|d| d < Duration::from_secs(3))
                .unwrap_or(false);
            let (dot, dot_color) = match status {
                Some(s) if s.last_error.is_some() => ("◎", Color::Yellow),
                Some(s) if s.active_connections > 0 => ("●", Color::Green),
                _ if in_cooldown => ("●", Color::Green),
                Some(s) if s.has_connector => ("○", Color::DarkGray),
                _ => ("○", Color::Gray),
            };
            let count = app.events.iter().filter(|ev| ev.component == c.name).count();
            let count_style = if count > 0 { Style::default().fg(Color::Green) } else { Style::default().fg(Color::DarkGray) };
            ListItem::new(Line::from(vec![
                Span::raw(format!("{} ", prefix)),
                Span::styled(dot, Style::default().fg(dot_color)),
                Span::styled(if c.listen.is_empty() { " ⊙" } else { " ⇄" }, Style::default().fg(Color::DarkGray)),
                Span::styled(format!(" {}", c.name), style),
                Span::styled(format!(" {}", count), count_style),
            ])).style(style)
        }));
    };

    let comp_title = format!(" Ocular v{} ", env!("CARGO_PKG_VERSION"));
    let comp_border = if comp_focused { Style::default().fg(Color::Cyan) } else { Style::default().fg(Color::DarkGray) };

    // Split component area: list + optional filter input at bottom
    if app.focus == Focus::ComponentFilter {
        let comp_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(chunks[0]);
        let left = List::new(items)
            .block(Block::default().borders(Borders::TOP).border_style(comp_border).title(comp_title));
        f.render_widget(left, comp_chunks[0]);
        let filter_line = Paragraph::new(Line::from(Span::styled(
            format!(" /{}▌", app.component_filter),
            Style::default().fg(Color::Rgb(255, 165, 0)),
        )));
        f.render_widget(filter_line, comp_chunks[1]);
    } else {
        let left = List::new(items)
            .block(Block::default().borders(Borders::TOP).border_style(comp_border).title(comp_title));
        f.render_widget(left, chunks[0]);
    }

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
        .map(|(idx, (orig_idx, ev, match_indices))| {
            let time = format_time(&ev.timestamp);
            let lat = format_latency(&ev.latency);
            let spans: Vec<Span> = app.event_format.segments.iter().flat_map(|seg| {
                match seg {
                    FormatSegment::Literal(s) => vec![Span::raw(s.clone())],
                    FormatSegment::Field { name, width } => {
                        let (raw, style) = match name.as_str() {
                            "index" => (format!("{}", orig_idx + 1), theme.line_number),
                            "time" => (time.clone(), theme.timestamp),
                            "component" => (ev.component.to_string(), if ev.system { Style::default().fg(Color::Red) } else { theme.component_style(ev.protocol) }),
                            "command" => {
                                let cmd_style = if ev.system { Style::default().fg(Color::Red) } else { theme.command };
                                if ev.protocol == ocular_protocol::Protocol::Http && !ev.response.is_empty() && !ev.system {
                                    let status_code = ev.response.split_whitespace().next().unwrap_or("");
                                    let color = match status_code.chars().next() {
                                        Some('2') => Color::Green,
                                        Some('3') => Color::Cyan,
                                        Some('4') => Color::Yellow,
                                        Some('5') => Color::Red,
                                        _ => Color::DarkGray,
                                    };
                                    let formatted = match width {
                                        Some(w) if *w > 0 => format!("{:>width$}", ev.command, width = *w as usize),
                                        Some(w) if *w < 0 => format!("{:<width$}", ev.command, width = (-*w) as usize),
                                        _ => ev.command.clone(),
                                    };
                                    return vec![
                                        Span::styled(format!("[{}] ", status_code), Style::default().fg(color)),
                                        Span::styled(formatted, cmd_style),
                                    ];
                                }
                                (ev.command.clone(), cmd_style)
                            },
                            "latency" => {
                                let style = if app.latency_threshold_ms.is_some_and(|t| ev.latency.as_secs_f64() * 1000.0 > t) {
                                    Style::default().fg(Color::Red)
                                } else {
                                    theme.latency
                                };
                                (lat.to_string(), style)
                            },
                            "process" => (ev.process.clone().unwrap_or_default(), theme.latency),
                            "src" => (ev.src.clone().unwrap_or_default(), Style::default().fg(Color::Blue)),
                            "dest" => (ev.dest.clone().unwrap_or_default(), Style::default().fg(Color::Cyan)),
                            _ => (String::new(), Style::default()),
                        };
                        let formatted = match width {
                            Some(w) if *w > 0 => format!("{:>width$}", raw, width = *w as usize),
                            Some(w) if *w < 0 => format!("{:<width$}", raw, width = (-*w) as usize),
                            _ => raw,
                        };
                        // Highlight matched chars in command field
                        if name == "command" && !match_indices.is_empty() {
                            let highlight = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
                            let chars: Vec<char> = formatted.chars().collect();
                            let mut result: Vec<Span> = Vec::new();
                            let mut i = 0;
                            while i < chars.len() {
                                if match_indices.contains(&i) {
                                    let start = i;
                                    while i < chars.len() && match_indices.contains(&i) { i += 1; }
                                    result.push(Span::styled(chars[start..i].iter().collect::<String>(), highlight));
                                } else {
                                    let start = i;
                                    while i < chars.len() && !match_indices.contains(&i) { i += 1; }
                                    result.push(Span::styled(chars[start..i].iter().collect::<String>(), style));
                                }
                            }
                            return result;
                        }
                        vec![Span::styled(formatted, style)]
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
    if !filter_info.is_empty() {
        title_spans.push(Span::styled(filter_info, Style::default().fg(Color::Rgb(255, 165, 0))));
    }
    if !count_info.is_empty() {
        title_spans.push(Span::raw(format!("{} ", count_info)));
    }
    let event_list = List::new(event_items)
        .block(Block::default().borders(Borders::TOP).border_style(events_border)
            .title(Line::from(title_spans)));
    f.render_widget(event_list, right[0]);

    // Detail panel
    let detail_focused = app.focus == Focus::Detail;
    let selected_event = filtered.get(app.selected).map(|(_, ev, _)| *ev);
    let (detail_table, detail_cmd, detail_meta): (Text, Text, Line) = if let Some(ev) = selected_event {
        let mut lines: Vec<Line> = Vec::new();
        let mut formatted_cmd;

        if ev.protocol == ocular_protocol::Protocol::Amqp {
            // AMQP: distinguish Publish (send) vs Deliver (receive) vs request-response
            let is_publish = ev.command.contains("Basic.Publish");
            let is_deliver = ev.command.contains("Basic.Deliver");
            formatted_cmd = ev.full_command.clone();
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
                formatted_cmd = ev.full_command.clone();
                lines.push(Line::from(Span::styled(formatted_cmd.clone(), Style::default().fg(Color::Cyan))));
                if !ev.response_detail.is_empty() {
                    lines.push(Line::from(""));
                    for rd in ev.response_detail.lines() {
                        lines.push(Line::from(rd.to_string()));
                    }
                }
            }
        } else {
            // MySQL / Postgres / Redis: request, response
            formatted_cmd = if ev.protocol == ocular_protocol::Protocol::Mysql {
                format_sql(&ev.full_command)
            } else {
                ev.full_command.clone()
            };
        }

        // Build metadata line
        let mut meta_parts: Vec<Span> = Vec::new();
        meta_parts.push(Span::raw(format!("{}  ", format_time(&ev.timestamp))));
        if let Some(s) = &ev.src {
            meta_parts.push(Span::styled(format!("{}  ", s), Style::default().fg(Color::Blue)));
        }
        if let Some(d) = &ev.dest {
            meta_parts.push(Span::styled(format!("→ {}  ", d), Style::default().fg(Color::Cyan)));
        }
        if let Some(p) = &ev.process {
            meta_parts.push(Span::styled(format!("{}  ", p), Style::default().fg(Color::DarkGray)));
        }
        if ev.latency.as_nanos() > 0 {
            meta_parts.push(Span::styled(format_latency(&ev.latency), Style::default().fg(Color::Yellow)));
        }
        let meta_line = Line::from(meta_parts);

        let mut table_lines: Vec<Line> = Vec::new();
        let mut in_json = false;
        for rd in ev.response_detail.lines() {
            if rd == "[Response Body]" || rd == "[Request Body]" {
                in_json = ev.protocol == ocular_protocol::Protocol::Http;
                table_lines.push(Line::from(Span::styled(rd.to_string(), Style::default().fg(Color::DarkGray))));
            } else if rd.starts_with('[') && rd.ends_with(']') {
                in_json = false;
                table_lines.push(Line::from(Span::styled(rd.to_string(), Style::default().fg(Color::DarkGray))));
            } else if in_json {
                table_lines.push(highlight_json_line(rd));
            } else {
                table_lines.push(Line::from(rd.to_string()));
            }
        }
        let mut sql_lines: Vec<Line> = Vec::new();
        for sql_line in formatted_cmd.lines() {
            sql_lines.push(highlight_sql_line(sql_line));
        }
        (Text::from(table_lines), Text::from(sql_lines), meta_line)
    } else {
        (Text::from(String::new()), Text::from("No events yet. Waiting for traffic..."), Line::from(String::new()))
    };
    let detail_text = detail_table;
    let detail_sql_text = detail_cmd;
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
let detail_panel_width = right[1].width.saturating_sub(2).max(1) as usize;

    // Build combined text: table (no wrap, on top) + SQL (pre-wrapped, below)
    let mut combined_lines: Vec<Line> = Vec::new();
    for rd in detail_text.lines {
        combined_lines.push(rd.clone());
    }
    if !detail_sql_text.lines.is_empty() {
        combined_lines.push(Line::from(String::new()));
        for sql_line in detail_sql_text.lines {
            let text: String = sql_line.spans.iter().map(|s| s.content.as_ref()).collect();
            if text.len() > detail_panel_width {
                for chunk in text.as_bytes().chunks(detail_panel_width) {
                    let wrapped = String::from_utf8_lossy(chunk).to_string();
                    combined_lines.push(Line::from(wrapped));
                }
            } else {
                combined_lines.push(sql_line.clone());
            }
        }
    }
    let combined_text = Text::from(combined_lines.clone());

    // Clamp scroll
    let scroll_text: String = combined_lines.iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
        .collect::<Vec<_>>().join("\n");
    let line_count: u16 = scroll_text.lines().count().max(1) as u16;
    let max_scroll = line_count.saturating_sub(1);
    app.detail_scroll = app.detail_scroll.min(max_scroll);

    // Split detail area: main content + 1-line sticky footer
    let detail_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(right[1]);

    let detail_widget = Paragraph::new(combined_text)
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
        let follow_span = if app.follow {
            Span::styled(" FOLLOW ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        } else {
            Span::raw("")
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
        let right_line = Line::from(vec![follow_span, mode_span]);
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
        let mut menu_lines = vec![
            Line::from(Span::styled(" Space Leader Menu", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
            Line::from(""),
        ];
        if !app.preview {
            menu_lines.push(Line::from(vec![Span::styled(" h", Style::default().fg(Color::Cyan)), Span::raw("  → Left panel")]));
        }
        menu_lines.push(Line::from(vec![Span::styled(" j", Style::default().fg(Color::Cyan)), Span::raw("  → Below panel")]));
        menu_lines.push(Line::from(vec![Span::styled(" k", Style::default().fg(Color::Cyan)), Span::raw("  → Above panel")]));
        if !app.preview {
            menu_lines.push(Line::from(vec![Span::styled(" l", Style::default().fg(Color::Cyan)), Span::raw("  → Right panel")]));
        }
        menu_lines.push(Line::from(vec![Span::styled(" c", Style::default().fg(Color::Cyan)), Span::raw("  → Clear all events")]));
        menu_lines.push(Line::from(vec![Span::styled(" f", Style::default().fg(Color::Cyan)), Span::raw("  → Toggle follow (tail -f)")]));
        menu_lines.push(Line::from(vec![Span::styled(" p", Style::default().fg(Color::Cyan)), Span::raw("  → Pause/resume stream")]));
        if !app.preview {
            menu_lines.push(Line::from(vec![Span::styled(" ,", Style::default().fg(Color::Cyan)), Span::raw("  → Edit config")]));
            menu_lines.push(Line::from(vec![Span::styled(" g", Style::default().fg(Color::Cyan)), Span::raw("  → Switch group")]));
        }
        menu_lines.push(Line::from(""));
        menu_lines.push(Line::from(Span::styled(" Esc/any  → cancel", Style::default().fg(Color::DarkGray))));
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
            Line::from(vec![Span::styled("  f         ", Style::default().fg(Color::Green)), Span::raw("Toggle follow")]),
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

    // Proxy form popup
    if let Some(ref form) = app.proxy_form {
        let area = f.area();
        let w: u16 = 42;
        let h: u16 = 13;
        let x = (area.width.saturating_sub(w)) / 2;
        let y = (area.height.saturating_sub(h)) / 2;
        let popup_area = Rect::new(x, y, w, h);
        f.render_widget(Clear, popup_area);

        let title = if form.editing_idx.is_some() { " Edit Proxy " } else { " New Proxy " };
        let protocol = PROTOCOLS[form.protocol_idx];
        let mode = MODES[form.mode_idx];
        let remote_default_port = default_port(protocol);

        // field_idx, label, value, placeholder
        let mut rows: Vec<(usize, &str, &str, &str)> = vec![
            (0, "name", form.inputs[0].value(), ""),
            (1, "protocol", protocol, ""),
            (2, "mode", mode, ""),
        ];
        if form.mode_idx == 1 {
            rows.push((3, "remote host", form.inputs[3].value(), "127.0.0.1"));
            rows.push((4, "remote port", form.inputs[4].value(), remote_default_port));
            rows.push((5, "interface", form.inputs[5].value(), DEFAULT_IFACE));
        } else if form.editing_idx.is_some() {
            rows.push((3, "listen host", form.inputs[1].value(), "127.0.0.1"));
            rows.push((4, "listen port", form.inputs[2].value(), ""));
            rows.push((5, "remote host", form.inputs[3].value(), "127.0.0.1"));
            rows.push((6, "remote port", form.inputs[4].value(), remote_default_port));
        } else {
            rows.push((3, "remote host", form.inputs[3].value(), "127.0.0.1"));
            rows.push((4, "remote port", form.inputs[4].value(), remote_default_port));
        }

        let mut lines: Vec<Line> = Vec::new();
        let max_value_width = (w as usize).saturating_sub(2 + 18 + 1); // borders + label prefix + padding
        for &(i, label, value, placeholder) in &rows {
            let is_active = i == form.active_field;
            let label_style = if is_active {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let hint = if (i == 1 || i == 2) && is_active { " ◀ ▶" } else { "" };
            let display = if value.is_empty() && !placeholder.is_empty() && !is_active {
                vec![
                    Span::styled(format!("   {:>12}: ", label), label_style),
                    Span::styled(placeholder.to_string(), Style::default().fg(Color::Rgb(80, 80, 80))),
                ]
            } else if value.is_empty() && !placeholder.is_empty() && is_active {
                vec![
                    Span::styled(format!("   {:>12}: ", label), label_style),
                    Span::styled("▌", Style::default().fg(Color::White)),
                    Span::styled(format!(" ({})", placeholder), Style::default().fg(Color::Rgb(80, 80, 80))),
                ]
            } else if is_active {
                // Show cursor at position within value
                let cur = form.field_idx().map(|fi| form.inputs[fi].cursor().min(value.len())).unwrap_or(value.len());
                // Scroll: keep cursor visible within max_value_width
                let (vis_start, vis_end) = if value.len() <= max_value_width {
                    (0, value.len())
                } else if cur <= max_value_width / 2 {
                    (0, max_value_width)
                } else if cur >= value.len() - max_value_width / 2 {
                    (value.len() - max_value_width, value.len())
                } else {
                    (cur - max_value_width / 2, cur + max_value_width / 2)
                };
                let before = &value[vis_start..cur];
                let after = &value[cur..vis_end];
                let prefix = if vis_start > 0 { "…" } else { "" };
                let suffix = if vis_end < value.len() { "…" } else { "" };
                vec![
                    Span::styled(format!("   {:>12}: ", label), label_style),
                    Span::styled(format!("{}{}", prefix, before), Style::default().fg(Color::White)),
                    Span::styled("▌", Style::default().fg(Color::Cyan)),
                    Span::styled(format!("{}{}", after, suffix), Style::default().fg(Color::White)),
                    Span::styled(hint.to_string(), Style::default().fg(Color::DarkGray)),
                ]
            } else {
                // Non-active: truncate from right
                let visible_value = if value.len() > max_value_width {
                    format!("{}…", &value[..max_value_width - 1])
                } else {
                    value.to_string()
                };
                vec![
                    Span::styled(format!("   {:>12}: ", label), label_style),
                    Span::styled(visible_value, Style::default().fg(Color::White)),
                    Span::styled(hint.to_string(), Style::default().fg(Color::DarkGray)),
                ]
            };
            lines.push(Line::from(display));
        }
        // Error line (if any)
        let error_line = form.error.as_ref().map(|err| Line::from(Span::styled(format!("   ⚠ {}", err), Style::default().fg(Color::Red))));
        let hint_line = Line::from(vec![
            Span::raw("   "),
            Span::styled("Esc", Style::default().fg(Color::DarkGray)),
            Span::raw(" cancel  "),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" submit"),
        ]);
        // Pad to pin error+hint at bottom (inner height = h - 2 for borders)
        let inner_h = (h - 2) as usize;
        let bottom_lines = if error_line.is_some() { 2 } else { 1 };
        let pad = inner_h.saturating_sub(lines.len() + bottom_lines);
        for _ in 0..pad {
            lines.push(Line::from(""));
        }
        if let Some(el) = error_line {
            lines.push(el);
        }
        lines.push(hint_line);

        let popup = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(title));
        f.render_widget(popup, popup_area);
    }

    // Delete confirm popup
    if let Some(idx) = app.delete_confirm_idx {
        let name = app.components.get(idx).map(|c| c.name.as_str()).unwrap_or("?");
        let area = f.area();
        let w: u16 = 36;
        let h: u16 = 5;
        let x = (area.width.saturating_sub(w)) / 2;
        let y = (area.height.saturating_sub(h)) / 2;
        let popup_area = Rect::new(x, y, w, h);
        f.render_widget(Clear, popup_area);
        let lines = vec![
            Line::from(format!(" Delete \"{}\"?", name)),
            Line::from(""),
            Line::from(vec![
                Span::raw(" "),
                Span::styled("y/Enter", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::raw(" confirm  "),
                Span::styled("n/Esc", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                Span::raw(" cancel"),
            ]),
        ];
        let popup = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
                .title(" Confirm Delete "));
        f.render_widget(popup, popup_area);
    }

    // Info popup
    if let Some(idx) = app.info_popup_idx {
        if let Some(ci) = app.components.get(idx) {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(" name:   ", Style::default().fg(Color::DarkGray)),
                    Span::styled(ci.name.clone(), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled(" listen: ", Style::default().fg(Color::DarkGray)),
                    Span::raw(ci.listen.clone()),
                ]),
            ];
            // Load protocol and remote from config
            if let Ok(content) = std::fs::read_to_string(&app.config_path) {
                if let Ok(cfg) = toml::from_str::<ReloadableConfig>(&content) {
                    if let Some(p) = cfg.proxy.iter().find(|p| p.name == ci.name) {
                        lines.insert(1, Line::from(vec![
                            Span::styled(" proto:  ", Style::default().fg(Color::DarkGray)),
                            Span::raw(p.protocol.clone()),
                        ]));
                        lines.push(Line::from(vec![
                            Span::styled(" remote: ", Style::default().fg(Color::DarkGray)),
                            Span::raw(p.remote.clone()),
                        ]));
                    }
                }
            }
            let stats = app.component_stats.get(&ci.name);
            let count = stats.map_or(0, |s| s.count);
            lines.push(Line::from(vec![
                Span::styled(" events: ", Style::default().fg(Color::DarkGray)),
                Span::styled(count.to_string(), Style::default().fg(Color::Yellow)),
            ]));
            if let Some(s) = stats {
                lines.push(Line::from(vec![
                    Span::styled(" qps:    ", Style::default().fg(Color::DarkGray)),
                    Span::raw(format!("{:.1}", s.qps())),
                ]));
                if !s.latencies.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled(" latency: ", Style::default().fg(Color::DarkGray)),
                        Span::raw(format!("min={:.2}ms avg={:.2}ms max={:.2}ms p95={:.2}ms",
                            s.latency_min.as_secs_f64() * 1000.0,
                            s.avg_latency().as_secs_f64() * 1000.0,
                            s.latency_max.as_secs_f64() * 1000.0,
                            s.p95_latency().as_secs_f64() * 1000.0,
                        )),
                    ]));
                }
                if s.error_count > 0 {
                    lines.push(Line::from(vec![
                        Span::styled(" errors: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(format!("{} ({:.1}%)", s.error_count, s.error_rate()), Style::default().fg(Color::Red)),
                    ]));
                }
            }

            let area = f.area();
            let w: u16 = 60;
            let h = lines.len() as u16 + 2;
            let x = (area.width.saturating_sub(w)) / 2;
            let y = (area.height.saturating_sub(h)) / 2;
            let popup_area = Rect::new(x, y, w, h);
            f.render_widget(Clear, popup_area);
            let popup = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan))
                    .title(" Proxy Info (i to close) "));
            f.render_widget(popup, popup_area);
        }
    }

    // Group picker popup
    if let Some(ref picker) = app.group_picker {
        let area = f.area();
        let h = (picker.groups.len() as u16 + 2).min(area.height - 4);
        let w: u16 = 30;
        let x = (area.width.saturating_sub(w)) / 2;
        let y = (area.height.saturating_sub(h)) / 2;
        let popup_area = Rect::new(x, y, w, h);
        f.render_widget(Clear, popup_area);
        let items: Vec<ListItem> = picker.groups.iter().enumerate().map(|(i, g)| {
            let is_active = app.active_group.as_deref() == Some(g.as_str());
            let prefix = if i == picker.selected { " ●" } else { "  " };
            let suffix = if is_active { " ✓" } else { "" };
            let style = if i == picker.selected {
                Style::default().bg(Color::DarkGray).fg(Color::White)
            } else if is_active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            ListItem::new(format!("{} {}{}", prefix, g, suffix)).style(style)
        }).collect();
        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(255, 165, 0)))
                .title(" Switch Group (j/k, Enter) "));
        f.render_widget(list, popup_area);
    }
}
