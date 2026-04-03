use ratatui::{
    prelude::*,
    widgets::*,
};

use crate::app::{App, Tab};

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // title bar
            Constraint::Min(0),   // main area
            Constraint::Length(1), // status bar
        ])
        .split(f.area());

    draw_title_bar(f, app, chunks[0]);
    draw_status_bar(f, app, chunks[2]);

    if app.show_detail {
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(chunks[1]);
        draw_message_list(f, app, main_chunks[0]);
        draw_detail_panel(f, app, main_chunks[1]);
    } else {
        draw_message_list(f, app, chunks[1]);
    }

    if app.show_filter_menu {
        draw_filter_popup(f, app);
    }
}

fn draw_title_bar(f: &mut Frame, app: &App, area: Rect) {
    let type_counts = app.type_counts();
    let counts_str: Vec<String> = type_counts
        .iter()
        .map(|(t, c)| format!("{t}: {c}"))
        .collect();
    let title = format!(
        " {} | {} messages | {} ",
        app.filename,
        app.total_messages,
        counts_str.join(" | ")
    );
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    f.render_widget(block, area);
}

fn draw_message_list(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .filtered_indices
        .iter()
        .enumerate()
        .map(|(_display_idx, &entry_idx)| {
            let entry = &app.all_entries[entry_idx];
            let style = match entry.raw.message_id {
                loguna::MessageId::Vision2014 => Style::default().fg(Color::Green),
                loguna::MessageId::Referee2013 => Style::default().fg(Color::Yellow),
                loguna::MessageId::VisionTracker2020 => Style::default().fg(Color::Blue),
                loguna::MessageId::Vision2010 => Style::default().fg(Color::DarkGray),
                _ => Style::default(),
            };
            let content = Line::from(Span::styled(&entry.summary, style));
            ListItem::new(content)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .title(format!(
                    " Messages [{}/{}] ",
                    if app.filtered_indices.is_empty() { 0 } else { app.selected + 1 },
                    app.filtered_indices.len()
                ))
                .borders(Borders::ALL),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut state = ListState::default();
    state.select(Some(app.selected));
    f.render_stateful_widget(list, area, &mut state);
}

fn draw_detail_panel(f: &mut Frame, app: &App, area: Rect) {
    let tabs = Tabs::new(vec!["Overview", "Raw Detail"])
        .select(match app.tab {
            Tab::Overview => 0,
            Tab::Detail => 1,
        })
        .style(Style::default().fg(Color::White))
        .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .divider(" | ");

    let tab_area = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    f.render_widget(tabs, tab_area[0]);

    if let Some(entry) = app.selected_entry() {
        let content = match app.tab {
            Tab::Overview => {
                let mut lines = vec![
                    format!("Message #{}", entry.index),
                    format!("Type: {}", entry.raw.message_id),
                    format!("Timestamp: {} ns", entry.raw.timestamp_ns),
                    format!("Relative: {:.6}s", (entry.raw.timestamp_ns - app.base_timestamp_ns) as f64 / 1e9),
                    format!("Payload Size: {} bytes", entry.raw.payload.len()),
                    String::new(),
                ];
                lines.push(entry.detail.clone());
                lines.join("\n")
            }
            Tab::Detail => {
                // Show hex dump of first 256 bytes
                let bytes = &entry.raw.payload;
                let mut lines = vec![format!("Raw payload ({} bytes):", bytes.len()), String::new()];
                for chunk in bytes.chunks(16).take(16) {
                    let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                    let ascii: String = chunk
                        .iter()
                        .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
                        .collect();
                    lines.push(format!("{:<48}  {}", hex.join(" "), ascii));
                }
                if bytes.len() > 256 {
                    lines.push(format!("... ({} more bytes)", bytes.len() - 256));
                }
                lines.join("\n")
            }
        };

        let paragraph = Paragraph::new(content)
            .block(
                Block::default()
                    .title(" Detail ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Magenta)),
            )
            .wrap(Wrap { trim: false });
        f.render_widget(paragraph, tab_area[1]);
    } else {
        let paragraph = Paragraph::new("No message selected")
            .block(
                Block::default()
                    .title(" Detail ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
        f.render_widget(paragraph, tab_area[1]);
    }
}

fn draw_filter_popup(f: &mut Frame, app: &App) {
    let area = centered_rect(40, 50, f.area());

    // Clear background
    f.render_widget(Clear, area);

    let type_counts = app.type_counts();
    let mut lines: Vec<Line> = vec![
        Line::from(Span::styled(
            "Toggle message filters:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    let filter_items = [
        ("1", loguna::MessageId::Vision2014, "Vision2014", Color::Green),
        ("2", loguna::MessageId::Referee2013, "Referee2013", Color::Yellow),
        ("3", loguna::MessageId::VisionTracker2020, "Tracker2020", Color::Blue),
        ("4", loguna::MessageId::Vision2010, "Vision2010", Color::DarkGray),
    ];

    for (key, msg_type, name, color) in &filter_items {
        let enabled = app.enabled_types.contains(msg_type);
        let check = if enabled { "✓" } else { " " };
        let count = type_counts
            .iter()
            .find(|(t, _)| t == msg_type)
            .map(|(_, c)| *c)
            .unwrap_or(0);
        lines.push(Line::from(vec![
            Span::styled(format!("[{check}] "), Style::default().fg(if enabled { Color::Green } else { Color::Red })),
            Span::styled(format!("{key}: "), Style::default().fg(Color::White)),
            Span::styled(format!("{name}"), Style::default().fg(*color)),
            Span::styled(format!(" ({count})"), Style::default().fg(Color::DarkGray)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press 'f' to close",
        Style::default().fg(Color::DarkGray),
    )));

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Filters ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black)),
    );

    f.render_widget(paragraph, area);
}

fn draw_status_bar(f: &mut Frame, _app: &App, area: Rect) {
    let help = " q:Quit  ↑↓/jk:Navigate  PgUp/PgDn:Page  Enter:Detail  Tab:Switch  f:Filters  1-4:Toggle ";
    let paragraph = Paragraph::new(help)
        .style(Style::default().fg(Color::White).bg(Color::DarkGray));
    f.render_widget(paragraph, area);
}

/// Helper function to create a centered rect.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
