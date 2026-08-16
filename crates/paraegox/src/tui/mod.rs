use std::error::Error;
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use paraegox_agent::{AgentConversationClient, CancelResult, SessionId, TurnId, TurnTerminal};
use paraegox_kernel::NodeId;
use ratatui::DefaultTerminal;
use tokio::io::{AsyncBufReadExt, BufReader};

mod app;
mod view;

use app::ChatApp;
use view::render_app;

type TuiResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

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
