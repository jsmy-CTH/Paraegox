use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};

use super::app::{AgentState, ChatApp, MessageRole};

const BACKGROUND: Color = Color::Rgb(0x0a, 0x0e, 0x13);
const CYAN: Color = Color::Rgb(0x37, 0xe8, 0xff);
const GREEN: Color = Color::Rgb(0x74, 0xff, 0x9c);
const DIM: Color = Color::Rgb(0x4a, 0x60, 0x68);
const FOREGROUND: Color = Color::Rgb(0xd7, 0xe3, 0xe7);
const YELLOW: Color = Color::Rgb(0xf4, 0xd3, 0x5e);
const RED: Color = Color::Rgb(0xff, 0x6b, 0x6b);

const THREE_COLUMN_WIDTH: u16 = 110;
const TWO_COLUMN_WIDTH: u16 = 80;
const SIDEBAR_HEIGHT: u16 = 24;
const MIN_TERMINAL_WIDTH: u16 = 40;
const MIN_TERMINAL_HEIGHT: u16 = 12;
const MAX_RENDERED_CHAT_LINES: usize = 512;

pub(super) fn render_app(frame: &mut Frame<'_>, app: &ChatApp) {
    let area = frame.area();
    frame.render_widget(
        Block::default().style(Style::default().bg(BACKGROUND)),
        area,
    );

    if area.width < MIN_TERMINAL_WIDTH || area.height < MIN_TERMINAL_HEIGHT {
        render_too_small(frame, area);
        return;
    }

    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);
    render_header(frame, app, sections[0]);
    render_body(frame, app, sections[1], area.width, area.height);
    render_input(frame, app, sections[2]);
    render_footer(frame, app, sections[3]);
}

fn render_too_small(frame: &mut Frame<'_>, area: Rect) {
    let prompt_area = Rect::new(
        area.x,
        area.y.saturating_add(area.height / 2),
        area.width,
        3,
    );
    let prompt = Paragraph::new(Text::from(vec![
        Line::styled(
            "PARAEGOX",
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
        Line::styled("TERMINAL TOO SMALL", Style::default().fg(FOREGROUND)),
        Line::styled("Resize to at least 40 × 12", Style::default().fg(DIM)),
    ]))
    .alignment(Alignment::Center)
    .style(Style::default().bg(BACKGROUND));
    frame.render_widget(prompt, prompt_area);
}

fn render_header(frame: &mut Frame<'_>, app: &ChatApp, area: Rect) {
    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            " PARAEGOX ",
            Style::default().fg(CYAN).add_modifier(Modifier::BOLD),
        ),
        Span::styled("DISTRIBUTED AGENT OS", Style::default().fg(DIM)),
        Span::styled("  →  ", Style::default().fg(DIM)),
        Span::styled(app.target.clone(), Style::default().fg(FOREGROUND)),
    ]))
    .block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(DIM)),
    )
    .style(Style::default().bg(BACKGROUND));
    frame.render_widget(header, area);
}

fn render_body(frame: &mut Frame<'_>, app: &ChatApp, area: Rect, width: u16, height: u16) {
    if height >= SIDEBAR_HEIGHT && width >= THREE_COLUMN_WIDTH {
        let columns = Layout::horizontal([
            Constraint::Length(25),
            Constraint::Min(40),
            Constraint::Length(28),
        ])
        .spacing(1)
        .split(area);
        render_session_panel(frame, app, columns[0]);
        render_chat(frame, app, columns[1]);
        render_target_panel(frame, app, columns[2]);
    } else if height >= SIDEBAR_HEIGHT && width >= TWO_COLUMN_WIDTH {
        let columns = Layout::horizontal([Constraint::Min(44), Constraint::Length(29)])
            .spacing(1)
            .split(area);
        render_chat(frame, app, columns[0]);
        render_target_panel(frame, app, columns[1]);
    } else {
        render_chat(frame, app, area);
    }
}

fn render_session_panel(frame: &mut Frame<'_>, app: &ChatApp, area: Rect) {
    let lines = vec![
        label_line("ID", app.session_id.to_string()),
        Line::raw(""),
        label_line("LIFETIME", "EPHEMERAL"),
        Line::styled("not persisted", Style::default().fg(DIM)),
        Line::raw(""),
        label_line("MESSAGES", app.messages.len().to_string()),
        label_line("TURNS", app.submitted_turns.to_string()),
    ];
    let panel = Paragraph::new(lines)
        .block(panel_block(" SESSION ", DIM))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(FOREGROUND).bg(BACKGROUND));
    frame.render_widget(panel, area);
}

fn render_target_panel(frame: &mut Frame<'_>, app: &ChatApp, area: Rect) {
    let (state, state_color) = state_label(app.agent_state);
    let lines = vec![
        label_line("NODE", app.target.clone()),
        Line::raw(""),
        Line::styled("ENDPOINT", Style::default().fg(DIM)),
        Line::styled(app.endpoint.clone(), Style::default().fg(FOREGROUND)),
        Line::raw(""),
        label_line("FABRIC SESSION", "OPEN"),
        Line::from(vec![
            Span::styled("AGENT REQUEST  ", Style::default().fg(DIM)),
            Span::styled(state, Style::default().fg(state_color)),
        ]),
        Line::raw(""),
        Line::styled("NON-STREAMING", Style::default().fg(DIM)),
    ];
    let panel = Paragraph::new(lines)
        .block(panel_block(" TARGET + AGENT ", DIM))
        .wrap(Wrap { trim: false })
        .style(Style::default().fg(FOREGROUND).bg(BACKGROUND));
    frame.render_widget(panel, area);
}

fn state_label(agent_state: AgentState) -> (&'static str, Color) {
    match agent_state {
        AgentState::Idle => ("IDLE", GREEN),
        AgentState::Waiting => ("WAITING", YELLOW),
        AgentState::Error => ("ERROR", RED),
    }
}

fn render_chat(frame: &mut Frame<'_>, app: &ChatApp, area: Rect) {
    let block = panel_block(" CHAT ", CYAN);
    let inner = block.inner(area);
    let lines = chat_lines(app);
    let scroll = chat_scroll(&lines, inner.width, inner.height);
    let chat = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0))
        .style(Style::default().fg(FOREGROUND).bg(BACKGROUND));
    frame.render_widget(chat, area);
}

fn chat_lines(app: &ChatApp) -> Vec<Line<'static>> {
    if app.messages.is_empty() {
        return vec![
            Line::styled(
                "Ready. Send a message to contact the Agent on this target.",
                Style::default().fg(DIM),
            ),
            Line::styled(
                "The current session is temporary and stays on the Agent service.",
                Style::default().fg(DIM),
            ),
        ];
    }

    let mut lines = VecDeque::with_capacity(MAX_RENDERED_CHAT_LINES);
    for message in &app.messages {
        let (label, label_color) = match message.role {
            MessageRole::User => ("YOU", CYAN),
            MessageRole::Agent => ("AGENT", GREEN),
            MessageRole::System => ("SYSTEM", DIM),
        };
        for (index, content) in message.content.split('\n').enumerate() {
            let prefix = if index == 0 { label } else { "" };
            push_render_line(
                &mut lines,
                Line::from(vec![
                    Span::styled(
                        format!("{prefix:<7}"),
                        Style::default()
                            .fg(label_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(content.to_owned(), Style::default().fg(FOREGROUND)),
                ]),
            );
        }
        push_render_line(&mut lines, Line::raw(""));
    }
    lines.into_iter().collect()
}

fn push_render_line(lines: &mut VecDeque<Line<'static>>, line: Line<'static>) {
    if lines.len() == MAX_RENDERED_CHAT_LINES {
        lines.pop_front();
    }
    lines.push_back(line);
}

fn chat_scroll(lines: &[Line<'_>], width: u16, height: u16) -> u16 {
    if width == 0 || height == 0 {
        return 0;
    }
    let width = usize::from(width);
    let visual_lines = lines.iter().fold(0usize, |count, line| {
        count.saturating_add(line.width().max(1).div_ceil(width))
    });
    visual_lines
        .saturating_sub(usize::from(height))
        .min(usize::from(u16::MAX)) as u16
}

fn render_input(frame: &mut Frame<'_>, app: &ChatApp, area: Rect) {
    let border_color = if app.agent_state == AgentState::Waiting {
        CYAN
    } else {
        GREEN
    };
    let block = panel_block(" MESSAGE ", border_color);
    let inner = block.inner(area);
    let (content, content_style) = if app.agent_state == AgentState::Waiting {
        ("Waiting for Agent…", Style::default().fg(DIM))
    } else if app.editor.text.is_empty() {
        ("Write a message…", Style::default().fg(DIM))
    } else {
        (app.editor.text.as_str(), Style::default().fg(FOREGROUND))
    };

    let cursor_width = Line::from(app.editor.prefix()).width();
    let available_width = usize::from(inner.width.saturating_sub(1));
    let horizontal_scroll = cursor_width.saturating_sub(available_width);
    let input = Paragraph::new(Span::styled(content, content_style))
        .block(block)
        .scroll((0, horizontal_scroll.min(usize::from(u16::MAX)) as u16))
        .style(Style::default().bg(BACKGROUND));
    frame.render_widget(input, area);

    if app.agent_state != AgentState::Waiting && inner.width > 0 && inner.height > 0 {
        let cursor_x = cursor_width.saturating_sub(horizontal_scroll);
        frame.set_cursor_position((inner.x.saturating_add(cursor_x as u16), inner.y));
    }
}

fn render_footer(frame: &mut Frame<'_>, app: &ChatApp, area: Rect) {
    let keys = if app.agent_state == AgentState::Waiting {
        " [Esc] cancel turn   [Ctrl-C] cancel + quit "
    } else {
        " [Enter] send   [Esc] clear   [Ctrl-C] quit   [/quit] quit "
    };
    let footer = Paragraph::new(Line::from(vec![
        Span::styled(keys, Style::default().fg(DIM)),
        Span::styled("│ ", Style::default().fg(DIM)),
        Span::styled(app.notice.clone(), Style::default().fg(FOREGROUND)),
    ]))
    .style(Style::default().bg(BACKGROUND));
    frame.render_widget(footer, area);
}

fn panel_block(title: &'static str, border_color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            title,
            Style::default()
                .fg(border_color)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(BACKGROUND))
}

fn label_line(label: &'static str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label}  "), Style::default().fg(DIM)),
        Span::styled(value.into(), Style::default().fg(FOREGROUND)),
    ])
}

#[cfg(test)]
mod tests {
    use paraegox_agent::SessionId;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::app::MAX_INPUT_BYTES;
    use super::*;

    #[test]
    fn responsive_layout_and_unicode_editor_preserve_the_bounded_chat_surface() {
        let mut app = ChatApp::new(
            "node-a".to_owned(),
            "tcp/127.0.0.1:7447".to_owned(),
            SessionId::new(),
        );
        assert!(app.editor.insert('你'));
        assert!(app.editor.insert('好'));
        app.editor.move_left();
        assert!(app.editor.insert('，'));
        app.editor.backspace();
        assert_eq!(app.editor.text, "你好");
        assert_eq!(app.editor.cursor, 1);
        app.editor.move_end();
        assert!(app.editor.insert_text(&"x".repeat(MAX_INPUT_BYTES - 6)));
        assert!(!app.editor.insert('x'));
        assert_eq!(app.editor.text.len(), MAX_INPUT_BYTES);
        app.editor.clear();
        app.push_message(MessageRole::User, "你好");
        app.push_message(MessageRole::Agent, "欢迎使用 Paraegox");

        let mut wide = Terminal::new(TestBackend::new(120, 30)).expect("test terminal");
        wide.draw(|frame| render_app(frame, &app))
            .expect("wide layout renders");
        let wide_text = rendered_text(wide.backend());
        assert!(wide_text.contains("SESSION"));
        assert!(wide_text.contains("CHAT"));
        assert!(wide_text.contains("TARGET + AGENT"));
        assert!(wide_text.contains('欢'));
        assert!(wide_text.contains("Paraegox"));

        let mut medium = Terminal::new(TestBackend::new(90, 26)).expect("test terminal");
        medium
            .draw(|frame| render_app(frame, &app))
            .expect("medium layout renders");
        let medium_text = rendered_text(medium.backend());
        assert!(!medium_text.contains("LIFETIME"));
        assert!(medium_text.contains("TARGET + AGENT"));

        let mut narrow = Terminal::new(TestBackend::new(70, 20)).expect("test terminal");
        narrow
            .draw(|frame| render_app(frame, &app))
            .expect("narrow layout renders");
        let narrow_text = rendered_text(narrow.backend());
        assert!(narrow_text.contains("CHAT"));
        assert!(!narrow_text.contains("TARGET + AGENT"));

        let mut tiny = Terminal::new(TestBackend::new(30, 8)).expect("test terminal");
        tiny.draw(|frame| render_app(frame, &app))
            .expect("too-small prompt renders");
        assert!(rendered_text(tiny.backend()).contains("TERMINAL TOO SMALL"));
    }

    fn rendered_text(backend: &TestBackend) -> String {
        backend
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }
}
