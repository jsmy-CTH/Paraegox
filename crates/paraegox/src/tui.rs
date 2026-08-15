use std::error::Error;
use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use paraegox_agent::{AgentConversationClient, SessionId, TurnId, TurnTerminal};
use paraegox_kernel::NodeId;
use tokio::io::{AsyncBufReadExt, BufReader};

pub(crate) async fn run_tui(
    target: NodeId,
    connect_endpoint: String,
    timeout: Duration,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let interactive = io::stdin().is_terminal();
    if interactive {
        println!("Paraegox chat target: {target}");
        println!("Type /quit to exit. Ctrl-C cancels the active turn.");
    }

    let session_id = SessionId::new();
    let mut client =
        AgentConversationClient::connect(&connect_endpoint, target, session_id, timeout).await?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    let conversation_result: Result<(), Box<dyn Error + Send + Sync>> = async {
        'conversation: loop {
            if interactive {
                print!("you> ");
                io::stdout().flush()?;
            }

            let input = tokio::select! {
                line = lines.next_line() => line?,
                signal = tokio::signal::ctrl_c() => {
                    signal?;
                    break 'conversation;
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
                    client.cancel(turn_id, timeout).await?;
                    break 'conversation;
                }
            };
            match result.terminal {
                TurnTerminal::Final { content } => println!("agent> {content}"),
                TurnTerminal::Cancelled => {
                    return Err(io::Error::other("Agent turn was cancelled").into());
                }
                TurnTerminal::TimedOut => {
                    return Err(
                        io::Error::new(io::ErrorKind::TimedOut, "Agent turn timed out").into(),
                    );
                }
                TurnTerminal::Failed { reason } => {
                    return Err(io::Error::other(reason).into());
                }
            }
            io::stdout().flush()?;
        }
        Ok(())
    }
    .await;

    let close_result = client.close(timeout).await;
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
