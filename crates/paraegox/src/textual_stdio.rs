//! Private, versioned stdio adapter for the bundled Textual client.

use std::error::Error;
use std::fmt::Display;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::time::Duration;

use paraegox_agent::{
    AgentConversationClient, AgentError, CancelResult, SessionId, TurnId, TurnResult, TurnTerminal,
};
use paraegox_kernel::NodeId;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

type AdapterResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

const PROTOCOL_VERSION: u8 = 1;
const MAX_INPUT_BYTES: usize = 4 * 1024;
const MAX_INPUT_FRAME_BYTES: usize = 16 * 1024;
const MAX_OUTPUT_FRAME_BYTES: usize = 64 * 1024;
const MAX_ERROR_BYTES: usize = 512;
const READ_CHUNK_BYTES: usize = 1024;
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const OUTPUT_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) async fn run_textual_stdio(
    target: NodeId,
    connect_endpoint: String,
    turn_timeout: Duration,
) -> AdapterResult {
    let session_id = SessionId::new();
    let mut client =
        AgentConversationClient::connect(&connect_endpoint, target, session_id, turn_timeout)
            .await?;
    let mut reader = BoundedJsonlReader::new(tokio::io::stdin());
    let mut output = JsonlOutput::new(tokio::io::stdout());

    if let Err(error) = output
        .send(&OutputFrame::Ready {
            v: PROTOCOL_VERSION,
            session_id,
            turn_timeout_ms: duration_millis(turn_timeout)?,
            max_input_bytes: MAX_INPUT_BYTES,
        })
        .await
    {
        let _ = client.close(CONTROL_TIMEOUT).await;
        return Err(error.into());
    }

    let protocol_result =
        run_protocol(&client, &mut reader, &mut output, session_id, turn_timeout).await;
    let close_result = client.close(CONTROL_TIMEOUT).await;

    match (protocol_result, close_result) {
        (Ok(ProtocolOutcome::Graceful), Ok(())) => {
            output
                .send(&OutputFrame::Stopped {
                    v: PROTOCOL_VERSION,
                    session_id,
                })
                .await?;
            Ok(())
        }
        (Ok(ProtocolOutcome::Fatal), Ok(())) => {
            Err(io::Error::other("JSONL conversation stopped after a fatal turn error").into())
        }
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

async fn run_protocol<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    client: &AgentConversationClient,
    reader: &mut BoundedJsonlReader<R>,
    output: &mut JsonlOutput<W>,
    session_id: SessionId,
    turn_timeout: Duration,
) -> AdapterResult<ProtocolOutcome> {
    loop {
        let Some(command) = reader.next_command().await? else {
            return Ok(ProtocolOutcome::Graceful);
        };
        command.ensure_version()?;

        match command {
            InputFrame::Submit {
                session_id: submitted_session,
                turn_id,
                input,
                ..
            } => {
                ensure_session(session_id, submitted_session)?;
                match drive_turn(
                    client,
                    reader,
                    output,
                    session_id,
                    turn_id,
                    &input,
                    turn_timeout,
                )
                .await?
                {
                    ActiveOutcome::Continue => {}
                    ActiveOutcome::GracefulShutdown => {
                        return Ok(ProtocolOutcome::Graceful);
                    }
                    ActiveOutcome::Fatal => return Ok(ProtocolOutcome::Fatal),
                }
            }
            InputFrame::Cancel {
                session_id: requested_session,
                turn_id,
                ..
            } => {
                ensure_session(session_id, requested_session)?;
                output
                    .send(&OutputFrame::CancelResult {
                        v: PROTOCOL_VERSION,
                        session_id,
                        turn_id,
                        result: CancelStatus::NotActive,
                    })
                    .await?;
            }
            InputFrame::Shutdown {
                session_id: requested_session,
                ..
            } => {
                ensure_session(session_id, requested_session)?;
                return Ok(ProtocolOutcome::Graceful);
            }
        }
    }
}

async fn drive_turn<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    client: &AgentConversationClient,
    reader: &mut BoundedJsonlReader<R>,
    output: &mut JsonlOutput<W>,
    session_id: SessionId,
    turn_id: TurnId,
    input: &str,
    turn_timeout: Duration,
) -> AdapterResult<ActiveOutcome> {
    let submission = client.submit_turn(turn_id, input, turn_timeout);
    tokio::pin!(submission);

    loop {
        tokio::select! {
            result = &mut submission => {
                match result {
                    Ok(result) => output
                        .send(&OutputFrame::TurnTerminal {
                            v: PROTOCOL_VERSION,
                            result,
                        })
                        .await?,
                    Err(error) => {
                        output
                            .send(&OutputFrame::TurnError {
                                v: PROTOCOL_VERSION,
                                session_id,
                                turn_id,
                                message: safe_message(error),
                            })
                            .await?;
                        return Ok(ActiveOutcome::Fatal);
                    }
                }
                return Ok(ActiveOutcome::Continue);
            }
            command = reader.next_command() => {
                let command = match command {
                    Ok(Some(command)) => command,
                    Ok(None) => {
                        return finish_active_shutdown(
                            client,
                            output,
                            session_id,
                            turn_id,
                            submission.as_mut(),
                        )
                        .await;
                    }
                    Err(error) => {
                        return fail_active_protocol(
                            client,
                            output,
                            session_id,
                            turn_id,
                            submission.as_mut(),
                            error,
                        )
                        .await;
                    }
                };
                if let Err(error) = command.ensure_version() {
                    return fail_active_protocol(
                        client,
                        output,
                        session_id,
                        turn_id,
                        submission.as_mut(),
                        error,
                    )
                    .await;
                }
                match command {
                    InputFrame::Cancel {
                        session_id: requested_session,
                        turn_id: requested_turn,
                        ..
                    } => {
                        if let Err(error) = ensure_session(session_id, requested_session) {
                            return fail_active_protocol(
                                client,
                                output,
                                session_id,
                                turn_id,
                                submission.as_mut(),
                                error,
                            )
                            .await;
                        }
                        if requested_turn != turn_id {
                            output
                                .send(&OutputFrame::CancelResult {
                                    v: PROTOCOL_VERSION,
                                    session_id,
                                    turn_id: requested_turn,
                                    result: CancelStatus::NotActive,
                                })
                                .await?;
                            continue;
                        }
                        let result = client.cancel(turn_id, CONTROL_TIMEOUT).await;
                        match result {
                            Ok(result) => {
                                let (status, terminal) = cancel_status(result);
                                output
                                    .send(&OutputFrame::CancelResult {
                                        v: PROTOCOL_VERSION,
                                        session_id,
                                        turn_id,
                                        result: status,
                                    })
                                    .await?;
                                if let Some(terminal) = terminal {
                                    output
                                        .send(&OutputFrame::TurnTerminal {
                                            v: PROTOCOL_VERSION,
                                            result: TurnResult {
                                                session_id,
                                                turn_id,
                                                terminal,
                                            },
                                        })
                                        .await?;
                                    return Ok(ActiveOutcome::Continue);
                                }
                            }
                            Err(error) => {
                                output
                                    .send(&OutputFrame::TurnError {
                                        v: PROTOCOL_VERSION,
                                        session_id,
                                        turn_id,
                                        message: safe_message(error),
                                    })
                                    .await?;
                                return Ok(ActiveOutcome::Fatal);
                            }
                        }
                    }
                    InputFrame::Submit {
                        session_id: submitted_session,
                        turn_id: submitted_turn,
                        ..
                    } => {
                        if let Err(error) = ensure_session(session_id, submitted_session) {
                            return fail_active_protocol(
                                client,
                                output,
                                session_id,
                                turn_id,
                                submission.as_mut(),
                                error,
                            )
                            .await;
                        }
                        output
                            .send(&OutputFrame::TurnError {
                                v: PROTOCOL_VERSION,
                                session_id,
                                turn_id: submitted_turn,
                                message: "another turn is already active".to_owned(),
                            })
                            .await?;
                        let _ = finish_active_shutdown(
                            client,
                            output,
                            session_id,
                            turn_id,
                            submission.as_mut(),
                        )
                        .await?;
                        return Ok(ActiveOutcome::Fatal);
                    }
                    InputFrame::Shutdown {
                        session_id: requested_session,
                        ..
                    } => {
                        if let Err(error) = ensure_session(session_id, requested_session) {
                            return fail_active_protocol(
                                client,
                                output,
                                session_id,
                                turn_id,
                                submission.as_mut(),
                                error,
                            )
                            .await;
                        }
                        return finish_active_shutdown(
                            client,
                            output,
                            session_id,
                            turn_id,
                            submission.as_mut(),
                        )
                        .await;
                    }
                }
            }
        }
    }
}

async fn fail_active_protocol<F, W>(
    client: &AgentConversationClient,
    output: &mut JsonlOutput<W>,
    session_id: SessionId,
    turn_id: TurnId,
    submission: Pin<&mut F>,
    protocol_error: io::Error,
) -> AdapterResult<ActiveOutcome>
where
    F: Future<Output = Result<TurnResult, AgentError>>,
    W: AsyncWrite + Unpin,
{
    let _ = finish_active_shutdown(client, output, session_id, turn_id, submission).await;
    Err(protocol_error.into())
}

async fn finish_active_shutdown<F, W>(
    client: &AgentConversationClient,
    output: &mut JsonlOutput<W>,
    session_id: SessionId,
    turn_id: TurnId,
    submission: Pin<&mut F>,
) -> AdapterResult<ActiveOutcome>
where
    F: Future<Output = Result<TurnResult, AgentError>>,
    W: AsyncWrite + Unpin,
{
    let confirmed_terminal = match client.cancel(turn_id, CONTROL_TIMEOUT).await {
        Ok(result) => {
            let (status, terminal) = cancel_status(result);
            output
                .send(&OutputFrame::CancelResult {
                    v: PROTOCOL_VERSION,
                    session_id,
                    turn_id,
                    result: status,
                })
                .await?;
            terminal
        }
        Err(error) => {
            output
                .send(&OutputFrame::TurnError {
                    v: PROTOCOL_VERSION,
                    session_id,
                    turn_id,
                    message: safe_message(error),
                })
                .await?;
            return Ok(ActiveOutcome::Fatal);
        }
    };

    if let Some(terminal) = confirmed_terminal {
        output
            .send(&OutputFrame::TurnTerminal {
                v: PROTOCOL_VERSION,
                result: TurnResult {
                    session_id,
                    turn_id,
                    terminal,
                },
            })
            .await?;
        return Ok(ActiveOutcome::GracefulShutdown);
    }

    match tokio::time::timeout(CONTROL_TIMEOUT, submission).await {
        Ok(Ok(result)) => {
            output
                .send(&OutputFrame::TurnTerminal {
                    v: PROTOCOL_VERSION,
                    result,
                })
                .await?;
            Ok(ActiveOutcome::GracefulShutdown)
        }
        Ok(Err(error)) => {
            output
                .send(&OutputFrame::TurnError {
                    v: PROTOCOL_VERSION,
                    session_id,
                    turn_id,
                    message: safe_message(error),
                })
                .await?;
            Ok(ActiveOutcome::Fatal)
        }
        Err(_) => {
            output
                .send(&OutputFrame::TurnError {
                    v: PROTOCOL_VERSION,
                    session_id,
                    turn_id,
                    message: "Agent terminal was not confirmed before shutdown".to_owned(),
                })
                .await?;
            Ok(ActiveOutcome::Fatal)
        }
    }
}

fn cancel_status(result: CancelResult) -> (CancelStatus, Option<TurnTerminal>) {
    match result {
        CancelResult::CancellationRequested => (CancelStatus::CancellationRequested, None),
        CancelResult::AlreadyTerminal { terminal } => {
            (CancelStatus::AlreadyTerminal, Some(terminal))
        }
        CancelResult::NotActive => (CancelStatus::NotActive, None),
    }
}

fn ensure_session(expected: SessionId, actual: SessionId) -> io::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "JSONL command does not match this session",
        ))
    }
}

fn duration_millis(duration: Duration) -> io::Result<u64> {
    u64::try_from(duration.as_millis())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "turn timeout is too large"))
}

fn safe_message(error: impl Display) -> String {
    truncate_utf8(&error.to_string(), MAX_ERROR_BYTES)
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut boundary = max_bytes;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum InputFrame {
    Submit {
        v: u8,
        session_id: SessionId,
        turn_id: TurnId,
        input: String,
    },
    Cancel {
        v: u8,
        session_id: SessionId,
        turn_id: TurnId,
    },
    Shutdown {
        v: u8,
        session_id: SessionId,
    },
}

impl InputFrame {
    fn ensure_version(&self) -> io::Result<()> {
        let version = match self {
            Self::Submit { v, .. } | Self::Cancel { v, .. } | Self::Shutdown { v, .. } => *v,
        };
        if version == PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported JSONL protocol version",
            ))
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OutputFrame {
    Ready {
        v: u8,
        session_id: SessionId,
        turn_timeout_ms: u64,
        max_input_bytes: usize,
    },
    CancelResult {
        v: u8,
        session_id: SessionId,
        turn_id: TurnId,
        result: CancelStatus,
    },
    TurnTerminal {
        v: u8,
        result: TurnResult,
    },
    TurnError {
        v: u8,
        session_id: SessionId,
        turn_id: TurnId,
        message: String,
    },
    Stopped {
        v: u8,
        session_id: SessionId,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CancelStatus {
    CancellationRequested,
    AlreadyTerminal,
    NotActive,
}

struct JsonlOutput<W> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> JsonlOutput<W> {
    fn new(writer: W) -> Self {
        Self { writer }
    }

    async fn send(&mut self, frame: &OutputFrame) -> io::Result<()> {
        let encoded = serde_json::to_vec(frame)
            .map_err(|_| io::Error::other("could not encode JSONL response"))?;
        if encoded.len() > MAX_OUTPUT_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSONL response exceeds 64 KiB",
            ));
        }
        tokio::time::timeout(OUTPUT_TIMEOUT, async {
            self.writer.write_all(&encoded).await?;
            self.writer.write_all(b"\n").await?;
            self.writer.flush().await
        })
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "JSONL output timed out"))?
    }
}

struct BoundedJsonlReader<R> {
    reader: R,
    buffered: Vec<u8>,
}

impl<R: AsyncRead + Unpin> BoundedJsonlReader<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            buffered: Vec::with_capacity(READ_CHUNK_BYTES),
        }
    }

    async fn next_command(&mut self) -> io::Result<Option<InputFrame>> {
        let Some(frame) = self.next_frame().await? else {
            return Ok(None);
        };
        serde_json::from_slice(&frame)
            .map(Some)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid JSONL command"))
    }

    async fn next_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(newline) = self.buffered.iter().position(|byte| *byte == b'\n') {
                if newline > MAX_INPUT_FRAME_BYTES {
                    return Err(frame_too_large());
                }
                let mut frame: Vec<u8> = self.buffered.drain(..=newline).collect();
                frame.pop();
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                return Ok(Some(frame));
            }
            if self.buffered.len() > MAX_INPUT_FRAME_BYTES {
                return Err(frame_too_large());
            }

            let remaining = MAX_INPUT_FRAME_BYTES + 1 - self.buffered.len();
            let mut chunk = [0_u8; READ_CHUNK_BYTES];
            let read = self
                .reader
                .read(&mut chunk[..remaining.min(READ_CHUNK_BYTES)])
                .await?;
            if read == 0 {
                if self.buffered.is_empty() {
                    return Ok(None);
                }
                if self.buffered.len() > MAX_INPUT_FRAME_BYTES {
                    return Err(frame_too_large());
                }
                let mut frame = std::mem::take(&mut self.buffered);
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                return Ok(Some(frame));
            }
            self.buffered.extend_from_slice(&chunk[..read]);
        }
    }
}

fn frame_too_large() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "JSONL command exceeds 16 KiB")
}

enum ProtocolOutcome {
    Graceful,
    Fatal,
}

enum ActiveOutcome {
    Continue,
    GracefulShutdown,
    Fatal,
}
