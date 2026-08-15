use std::collections::VecDeque;
use std::error::Error;
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use paraegox_agent::{AgentConversationClient, CancelResult, SessionId, TurnId, TurnTerminal};
use paraegox_kernel::NodeId;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use tokio::io::{AsyncBufReadExt, BufReader};

type TuiResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

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

const MAX_INPUT_BYTES: usize = 4 * 1024;
const MAX_UI_MESSAGES: usize = 64;
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_NOTICE_BYTES: usize = 512;
const MAX_ENDPOINT_BYTES: usize = 256;
const MAX_RENDERED_CHAT_LINES: usize = 512;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) async fn run_tui(
    target: NodeId,
    connect_endpoint: String,
    timeout: Duration,
) -> TuiResult {
    let fullscreen = io::stdin().is_terminal() && io::stdout().is_terminal();
    let target_label = target.to_string();
    let session_id = SessionId::new();
    let mut client =
        AgentConversationClient::connect(&connect_endpoint, target, session_id, timeout).await?;

    let conversation_result = if fullscreen {
        run_fullscreen_conversation(&client, target_label, connect_endpoint, session_id, timeout)
            .await
    } else {
        run_line_conversation(&client, timeout).await
    };

    let close_result = client.close(control_timeout(timeout)).await;
    match (conversation_result, close_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(error), Err(close_error)) => Err(io::Error::other(format!(
            "{error}; closing the Agent conversation also failed: {close_error}"
        ))
        .into()),
    }
}

async fn run_line_conversation(client: &AgentConversationClient, timeout: Duration) -> TuiResult {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    loop {
        let input = tokio::select! {
            line = lines.next_line() => line?,
            signal = tokio::signal::ctrl_c() => {
                signal?;
                break;
            }
        };
        let Some(input) = input else {
            break;
        };
        if input == "/quit" {
            break;
        }
        if input.trim().is_empty() {
            continue;
        }

        let turn_id = TurnId::new();
        let result = tokio::select! {
            result = client.submit_turn(turn_id, &input, timeout) => result?,
            signal = tokio::signal::ctrl_c() => {
                signal?;
                client.cancel(turn_id, control_timeout(timeout)).await?;
                break;
            }
        };
        match result.terminal {
            TurnTerminal::Final { content } => println!("agent> {content}"),
            TurnTerminal::Cancelled => {
                return Err(io::Error::other("Agent turn was cancelled").into());
            }
            TurnTerminal::TimedOut => {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "Agent turn timed out").into());
            }
            TurnTerminal::Failed { reason } => {
                return Err(io::Error::other(reason).into());
            }
        }
        io::stdout().flush()?;
    }

    Ok(())
}

async fn run_fullscreen_conversation(
    client: &AgentConversationClient,
    target: String,
    endpoint: String,
    session_id: SessionId,
    timeout: Duration,
) -> TuiResult {
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            ratatui::restore();
            return Err(error.into());
        }
    };
    let mut restore = TerminalRestore::new();
    let mut events = EventStream::new();
    let mut app = ChatApp::new(target, endpoint, session_id);

    let conversation_result =
        fullscreen_event_loop(&mut terminal, &mut events, client, &mut app, timeout).await;
    let restore_result = restore.finish();
    match (conversation_result, restore_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Err(error), Err(restore_error)) => Err(io::Error::other(format!(
            "{error}; restoring the terminal also failed: {restore_error}"
        ))
        .into()),
    }
}

async fn fullscreen_event_loop(
    terminal: &mut DefaultTerminal,
    events: &mut EventStream,
    client: &AgentConversationClient,
    app: &mut ChatApp,
    timeout: Duration,
) -> TuiResult {
    loop {
        terminal.draw(|frame| render_app(frame, app))?;
        let event = next_terminal_event(events).await?;
        match handle_idle_event(app, event) {
            IdleAction::Continue => {}
            IdleAction::Quit => break,
            IdleAction::Submit(input) => {
                let turn_id = TurnId::new();
                app.begin_turn(&input);
                if drive_active_turn(terminal, events, client, app, turn_id, &input, timeout)
                    .await?
                    .is_quit()
                {
                    break;
                }
            }
        }
    }
    Ok(())
}

async fn drive_active_turn(
    terminal: &mut DefaultTerminal,
    events: &mut EventStream,
    client: &AgentConversationClient,
    app: &mut ChatApp,
    turn_id: TurnId,
    input: &str,
    timeout: Duration,
) -> TuiResult<ActiveOutcome> {
    let submission = client.submit_turn(turn_id, input, timeout);
    tokio::pin!(submission);

    loop {
        terminal.draw(|frame| render_app(frame, app))?;
        tokio::select! {
            result = &mut submission => {
                app.finish_turn(result?.terminal);
                return Ok(ActiveOutcome::Continue);
            }
            event = next_terminal_event(events) => {
                match handle_active_event(event?) {
                    ActiveAction::Continue => {}
                    ActiveAction::Cancel => {
                        match client.cancel(turn_id, control_timeout(timeout)).await? {
                            CancelResult::CancellationRequested => {
                                app.cancellation_requested();
                            }
                            CancelResult::AlreadyTerminal { terminal } => {
                                app.finish_turn(terminal);
                                return Ok(ActiveOutcome::Continue);
                            }
                            CancelResult::NotActive => {
                                app.set_notice("Cancellation is not active yet; still waiting");
                            }
                        }
                    }
                    ActiveAction::Quit => {
                        match client.cancel(turn_id, control_timeout(timeout)).await? {
                            CancelResult::CancellationRequested => {
                                app.cancellation_requested();
                                return Ok(ActiveOutcome::Quit);
                            }
                            CancelResult::AlreadyTerminal { terminal } => {
                                app.finish_turn(terminal);
                                return Ok(ActiveOutcome::Quit);
                            }
                            CancelResult::NotActive => {
                                return Ok(ActiveOutcome::Quit);
                            }
                        }
                    }
                }
            }
        }
    }
}

async fn next_terminal_event(events: &mut EventStream) -> io::Result<Event> {
    match events.next().await {
        Some(event) => event,
        None => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "terminal event stream closed",
        )),
    }
}

fn control_timeout(turn_timeout: Duration) -> Duration {
    turn_timeout.min(CONTROL_TIMEOUT)
}

enum IdleAction {
    Continue,
    Submit(String),
    Quit,
}

enum ActiveAction {
    Continue,
    Cancel,
    Quit,
}

enum ActiveOutcome {
    Continue,
    Quit,
}

impl ActiveOutcome {
    fn is_quit(&self) -> bool {
        matches!(self, Self::Quit)
    }
}

fn handle_idle_event(app: &mut ChatApp, event: Event) -> IdleAction {
    match event {
        Event::Key(key) if is_key_input(&key) => {
            if is_ctrl_c(&key) {
                return IdleAction::Quit;
            }
            match key.code {
                KeyCode::Enter => {
                    if app.editor.text == "/quit" {
                        IdleAction::Quit
                    } else if app.editor.text.trim().is_empty() {
                        IdleAction::Continue
                    } else {
                        IdleAction::Submit(app.editor.take())
                    }
                }
                KeyCode::Esc => {
                    app.editor.clear();
                    app.set_notice("Input cleared");
                    IdleAction::Continue
                }
                KeyCode::Backspace => {
                    app.editor.backspace();
                    IdleAction::Continue
                }
                KeyCode::Delete => {
                    app.editor.delete();
                    IdleAction::Continue
                }
                KeyCode::Left => {
                    app.editor.move_left();
                    IdleAction::Continue
                }
                KeyCode::Right => {
                    app.editor.move_right();
                    IdleAction::Continue
                }
                KeyCode::Home => {
                    app.editor.move_home();
                    IdleAction::Continue
                }
                KeyCode::End => {
                    app.editor.move_end();
                    IdleAction::Continue
                }
                KeyCode::Char(character)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if !app.editor.insert(character) {
                        app.set_notice("Input is limited to 4 KiB");
                    }
                    IdleAction::Continue
                }
                _ => IdleAction::Continue,
            }
        }
        Event::Paste(text) => {
            if !app.editor.insert_text(&text) {
                app.set_notice("Input is limited to 4 KiB");
            }
            IdleAction::Continue
        }
        _ => IdleAction::Continue,
    }
}

fn handle_active_event(event: Event) -> ActiveAction {
    match event {
        Event::Key(key) if is_key_input(&key) && is_ctrl_c(&key) => ActiveAction::Quit,
        Event::Key(key) if is_key_input(&key) && key.code == KeyCode::Esc => ActiveAction::Cancel,
        _ => ActiveAction::Continue,
    }
}

fn is_key_input(key: &KeyEvent) -> bool {
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'C'))
}

#[derive(Clone, Copy)]
enum MessageRole {
    User,
    Agent,
    System,
}

struct ChatMessage {
    role: MessageRole,
    content: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum AgentState {
    Idle,
    Waiting,
    Error,
}

struct ChatApp {
    target: String,
    endpoint: String,
    session_id: SessionId,
    messages: VecDeque<ChatMessage>,
    editor: InputEditor,
    agent_state: AgentState,
    submitted_turns: u64,
    notice: String,
}

impl ChatApp {
    fn new(target: String, endpoint: String, session_id: SessionId) -> Self {
        Self {
            target: bounded_display_text(&target, 64),
            endpoint: bounded_display_text(&endpoint, MAX_ENDPOINT_BYTES),
            session_id,
            messages: VecDeque::new(),
            editor: InputEditor::default(),
            agent_state: AgentState::Idle,
            submitted_turns: 0,
            notice: "Fabric session open; Agent availability is confirmed by a reply".to_owned(),
        }
    }

    fn begin_turn(&mut self, input: &str) {
        self.push_message(MessageRole::User, input);
        self.agent_state = AgentState::Waiting;
        self.submitted_turns = self.submitted_turns.saturating_add(1);
        self.set_notice("Waiting for the Agent response");
    }

    fn finish_turn(&mut self, terminal: TurnTerminal) {
        match terminal {
            TurnTerminal::Final { content } => {
                self.push_message(MessageRole::Agent, &content);
                self.agent_state = AgentState::Idle;
                self.set_notice("Agent reply received");
            }
            TurnTerminal::Cancelled => {
                self.push_message(MessageRole::System, "Turn cancelled");
                self.agent_state = AgentState::Idle;
                self.set_notice("The active turn was cancelled");
            }
            TurnTerminal::TimedOut => {
                self.push_message(MessageRole::System, "Turn timed out");
                self.agent_state = AgentState::Error;
                self.set_notice("The last turn timed out");
            }
            TurnTerminal::Failed { reason } => {
                self.push_message(MessageRole::System, &format!("Turn failed: {reason}"));
                self.agent_state = AgentState::Error;
                self.set_notice("The last turn failed");
            }
        }
    }

    fn cancellation_requested(&mut self) {
        self.push_message(MessageRole::System, "Cancellation requested");
        self.set_notice("The active turn cancellation was accepted");
    }

    fn push_message(&mut self, role: MessageRole, content: &str) {
        if self.messages.len() == MAX_UI_MESSAGES {
            self.messages.pop_front();
        }
        self.messages.push_back(ChatMessage {
            role,
            content: bounded_display_text(content, MAX_MESSAGE_BYTES),
        });
    }

    fn set_notice(&mut self, notice: &str) {
        self.notice = bounded_display_text(notice, MAX_NOTICE_BYTES);
    }

    fn state_label(&self) -> (&'static str, Color) {
        match self.agent_state {
            AgentState::Idle => ("IDLE", GREEN),
            AgentState::Waiting => ("WAITING", YELLOW),
            AgentState::Error => ("ERROR", RED),
        }
    }
}

#[derive(Default)]
struct InputEditor {
    text: String,
    cursor: usize,
}

impl InputEditor {
    fn insert(&mut self, character: char) -> bool {
        if character.is_control() || self.text.len() + character.len_utf8() > MAX_INPUT_BYTES {
            return false;
        }
        let index = self.byte_index(self.cursor);
        self.text.insert(index, character);
        self.cursor += 1;
        true
    }

    fn insert_text(&mut self, text: &str) -> bool {
        let mut complete = true;
        for character in text.chars().filter(|character| !character.is_control()) {
            if !self.insert(character) {
                complete = false;
                break;
            }
        }
        complete
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_index(self.cursor - 1);
        let end = self.byte_index(self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        if self.cursor == self.char_count() {
            return;
        }
        let start = self.byte_index(self.cursor);
        let end = self.byte_index(self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_count());
    }

    fn move_home(&mut self) {
        self.cursor = 0;
    }

    fn move_end(&mut self) {
        self.cursor = self.char_count();
    }

    fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    fn prefix(&self) -> &str {
        &self.text[..self.byte_index(self.cursor)]
    }

    fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    fn byte_index(&self, character_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(character_index)
            .map_or(self.text.len(), |(index, _)| index)
    }
}

fn bounded_display_text(value: &str, max_bytes: usize) -> String {
    let mut output = String::with_capacity(value.len().min(max_bytes));
    let mut truncated = false;
    for character in value.chars() {
        let character = if character == '\n' {
            character
        } else if character.is_control() {
            '\u{fffd}'
        } else {
            character
        };
        if output.len() + character.len_utf8() > max_bytes.saturating_sub(3) {
            truncated = true;
            break;
        }
        output.push(character);
    }
    if truncated {
        output.push('…');
    }
    output
}

fn render_app(frame: &mut Frame<'_>, app: &ChatApp) {
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
    let (state, state_color) = app.state_label();
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

struct TerminalRestore {
    armed: bool,
}

impl TerminalRestore {
    fn new() -> Self {
        Self { armed: true }
    }

    fn finish(&mut self) -> io::Result<()> {
        let result = ratatui::try_restore();
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        if self.armed {
            ratatui::restore();
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

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
