use std::error::Error;
use std::io::{self, IsTerminal, Write};
use std::process::ExitCode;
use std::str::FromStr;
use std::time::Duration;

use paraegox_agent::{AgentConversationClient, SessionId, TurnId, TurnTerminal};
use paraegox_kernel::{NodeId, RuntimeHostId};
use paraegox_node::{Node, NodeIdentity, probe_node};
use paraegox_runtime::RuntimeHostIdentity;
use tokio::io::{AsyncBufReadExt, BufReader};

const DEFAULT_ENDPOINT: &str = "tcp/127.0.0.1:7447";
const DEFAULT_PROBE_TIMEOUT_MS: u64 = 2_000;

const HELP: &str = "Paraegox — distributed embodied-intelligence Agent OS

Usage:
  paraegox --help
  paraegox --version
  paraegox node run --node-id <id> [--deck builtin-agent] [--listen <loopback-tcp-endpoint>] [--connect <loopback-tcp-endpoint> --probe-peer <id> [--timeout-ms <ms>]]
  paraegox node probe --target <id> [--connect <loopback-tcp-endpoint>] [--timeout-ms <ms>]
  paraegox tui --target <id> [--connect <loopback-tcp-endpoint>] [--timeout-ms <ms>]

Current capability:
  Two addressable Nodes can run on same-host loopback. A Node can also run the built-in Agent Deck for an independent terminal conversation.

Defaults:
  --listen / --connect  tcp/127.0.0.1:7447
  --timeout-ms          2000";

#[tokio::main]
async fn main() -> ExitCode {
    match parse_command(std::env::args().skip(1).collect()) {
        Ok(Command::Help) => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Ok(Command::Version) => {
            println!("paraegox {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Command::Run {
            node_id,
            listen_endpoint,
            connect_endpoint,
            peer,
            timeout,
            deck,
        }) => finish(
            run_node(
                node_id,
                listen_endpoint,
                connect_endpoint,
                peer,
                timeout,
                deck,
            )
            .await,
        ),
        Ok(Command::Probe {
            target,
            connect_endpoint,
            timeout,
        }) => finish(run_probe(target, connect_endpoint, timeout).await),
        Ok(Command::Tui {
            target,
            connect_endpoint,
            timeout,
        }) => finish(run_tui(target, connect_endpoint, timeout).await),
        Err(error) => {
            eprintln!("error: {error}\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}

fn finish(result: Result<(), Box<dyn Error + Send + Sync>>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run_node(
    node_id: NodeId,
    listen_endpoint: String,
    connect_endpoint: Option<String>,
    peer: Option<NodeId>,
    timeout: Duration,
    deck: Option<DeckSelection>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let identity = NodeIdentity::new(node_id);
    let runtime_host_id = RuntimeHostId::new(format!("{}:runtime-0", identity.node_id))?;
    let runtime_identity = RuntimeHostIdentity::new(runtime_host_id);
    let mut node = match deck {
        Some(DeckSelection::BuiltinAgent) => Node::new_with_builtin_agent(
            identity,
            runtime_identity,
            listen_endpoint,
            connect_endpoint,
        )?,
        None => Node::new(
            identity,
            runtime_identity,
            listen_endpoint,
            connect_endpoint,
        )?,
    };

    node.start().await?;
    let running_result: Result<(), Box<dyn Error + Send + Sync>> = async {
        println!("{}", serde_json::to_string(&node.status()?)?);
        io::stdout().flush()?;

        if let Some(target) = peer.as_ref() {
            let status = node.probe_peer(target, timeout).await?;
            println!(
                "{}",
                serde_json::json!({
                    "peer": status,
                })
            );
            io::stdout().flush()?;
        }

        tokio::signal::ctrl_c().await?;
        Ok(())
    }
    .await;

    let stop_result = node.stop().await;
    match (running_result, stop_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(running_error), Ok(())) => Err(running_error),
        (Ok(()), Err(stop_error)) => Err(stop_error.into()),
        (Err(running_error), Err(stop_error)) => Err(io::Error::other(format!(
            "{running_error}; stopping the local Node also failed: {stop_error}"
        ))
        .into()),
    }
}

async fn run_probe(
    target: NodeId,
    connect_endpoint: String,
    timeout: Duration,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let status = probe_node(&target, &connect_endpoint, timeout).await?;
    println!("{}", serde_json::to_string(&status)?);
    Ok(())
}

async fn run_tui(
    target: NodeId,
    connect_endpoint: String,
    timeout: Duration,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let session_id = SessionId::new();
    let mut client =
        AgentConversationClient::connect(&connect_endpoint, target, session_id, timeout).await?;
    let interactive = io::stdin().is_terminal();
    let mut lines = BufReader::new(tokio::io::stdin()).lines();

    let conversation_result: Result<(), Box<dyn Error + Send + Sync>> = async {
        'conversation: loop {
            if interactive {
                print!("> ");
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

#[derive(Clone, Copy)]
enum DeckSelection {
    BuiltinAgent,
}

enum Command {
    Help,
    Version,
    Run {
        node_id: NodeId,
        listen_endpoint: String,
        connect_endpoint: Option<String>,
        peer: Option<NodeId>,
        timeout: Duration,
        deck: Option<DeckSelection>,
    },
    Probe {
        target: NodeId,
        connect_endpoint: String,
        timeout: Duration,
    },
    Tui {
        target: NodeId,
        connect_endpoint: String,
        timeout: Duration,
    },
}

fn parse_command(arguments: Vec<String>) -> Result<Command, String> {
    match arguments.as_slice() {
        [] => Ok(Command::Help),
        [flag] if matches!(flag.as_str(), "--help" | "-h") => Ok(Command::Help),
        [flag] if matches!(flag.as_str(), "--version" | "-V") => Ok(Command::Version),
        [scope, operation, rest @ ..] if scope == "node" && operation == "run" => parse_run(rest),
        [scope, operation, rest @ ..] if scope == "node" && operation == "probe" => {
            parse_probe(rest)
        }
        [scope, rest @ ..] if scope == "tui" => parse_tui(rest),
        [argument, ..] => Err(format!("unknown argument or command `{argument}`")),
    }
}

fn parse_run(arguments: &[String]) -> Result<Command, String> {
    let mut node_id = None;
    let mut listen_endpoint = DEFAULT_ENDPOINT.to_owned();
    let mut listen_was_set = false;
    let mut connect_endpoint = None;
    let mut peer = None;
    let mut timeout_ms = DEFAULT_PROBE_TIMEOUT_MS;
    let mut timeout_was_set = false;
    let mut deck = None;
    let mut index = 0;

    while index < arguments.len() {
        let (name, value) = option_pair(arguments, index)?;
        match name {
            "--node-id" if node_id.is_none() => {
                node_id = Some(NodeId::from_str(value).map_err(|error| error.to_string())?);
            }
            "--listen" if !listen_was_set => {
                listen_endpoint = value.to_owned();
                listen_was_set = true;
            }
            "--connect" if connect_endpoint.is_none() => {
                connect_endpoint = Some(value.to_owned());
            }
            "--probe-peer" if peer.is_none() => {
                peer = Some(NodeId::from_str(value).map_err(|error| error.to_string())?);
            }
            "--timeout-ms" if !timeout_was_set => {
                timeout_ms = parse_timeout_ms(value)?;
                timeout_was_set = true;
            }
            "--deck" if deck.is_none() => {
                deck = Some(match value {
                    "builtin-agent" => DeckSelection::BuiltinAgent,
                    _ => return Err(format!("unknown Deck `{value}`; expected `builtin-agent`")),
                });
            }
            "--node-id" | "--listen" | "--connect" | "--probe-peer" | "--timeout-ms" | "--deck" => {
                return Err(format!("duplicate option `{name}`"));
            }
            _ => return Err(format!("unknown node run option `{name}`")),
        }
        index += 2;
    }

    let node_id = node_id.ok_or_else(|| "node run requires --node-id <id>".to_owned())?;
    match (&connect_endpoint, &peer) {
        (Some(_), Some(_)) => {}
        (None, None) if !timeout_was_set => {}
        (None, None) => {
            return Err("node run --timeout-ms requires --connect and --probe-peer".to_owned());
        }
        _ => {
            return Err("node run --connect and --probe-peer must be used together".to_owned());
        }
    }
    if peer.as_ref() == Some(&node_id) {
        return Err("node run --probe-peer must name a different Node".to_owned());
    }
    Ok(Command::Run {
        node_id,
        listen_endpoint,
        connect_endpoint,
        peer,
        timeout: Duration::from_millis(timeout_ms),
        deck,
    })
}

fn parse_probe(arguments: &[String]) -> Result<Command, String> {
    let mut target = None;
    let mut connect_endpoint = DEFAULT_ENDPOINT.to_owned();
    let mut timeout_ms = DEFAULT_PROBE_TIMEOUT_MS;
    let mut connect_was_set = false;
    let mut timeout_was_set = false;
    let mut index = 0;

    while index < arguments.len() {
        let (name, value) = option_pair(arguments, index)?;
        match name {
            "--target" if target.is_none() => {
                target = Some(NodeId::from_str(value).map_err(|error| error.to_string())?);
            }
            "--connect" if !connect_was_set => {
                connect_endpoint = value.to_owned();
                connect_was_set = true;
            }
            "--timeout-ms" if !timeout_was_set => {
                timeout_ms = parse_timeout_ms(value)?;
                timeout_was_set = true;
            }
            "--target" | "--connect" | "--timeout-ms" => {
                return Err(format!("duplicate option `{name}`"));
            }
            _ => return Err(format!("unknown node probe option `{name}`")),
        }
        index += 2;
    }

    let target = target.ok_or_else(|| "node probe requires --target <id>".to_owned())?;
    Ok(Command::Probe {
        target,
        connect_endpoint,
        timeout: Duration::from_millis(timeout_ms),
    })
}

fn parse_tui(arguments: &[String]) -> Result<Command, String> {
    let mut target = None;
    let mut connect_endpoint = DEFAULT_ENDPOINT.to_owned();
    let mut timeout_ms = DEFAULT_PROBE_TIMEOUT_MS;
    let mut connect_was_set = false;
    let mut timeout_was_set = false;
    let mut index = 0;

    while index < arguments.len() {
        let (name, value) = option_pair(arguments, index)?;
        match name {
            "--target" if target.is_none() => {
                target = Some(NodeId::from_str(value).map_err(|error| error.to_string())?);
            }
            "--connect" if !connect_was_set => {
                connect_endpoint = value.to_owned();
                connect_was_set = true;
            }
            "--timeout-ms" if !timeout_was_set => {
                timeout_ms = parse_timeout_ms(value)?;
                timeout_was_set = true;
            }
            "--target" | "--connect" | "--timeout-ms" => {
                return Err(format!("duplicate option `{name}`"));
            }
            _ => return Err(format!("unknown tui option `{name}`")),
        }
        index += 2;
    }

    let target = target.ok_or_else(|| "tui requires --target <id>".to_owned())?;
    Ok(Command::Tui {
        target,
        connect_endpoint,
        timeout: Duration::from_millis(timeout_ms),
    })
}

fn parse_timeout_ms(value: &str) -> Result<u64, String> {
    let timeout_ms = value
        .parse::<u64>()
        .map_err(|_| "--timeout-ms must be an integer".to_owned())?;
    if !(100..=60_000).contains(&timeout_ms) {
        return Err("--timeout-ms must be between 100 and 60000".to_owned());
    }
    Ok(timeout_ms)
}

fn option_pair(arguments: &[String], index: usize) -> Result<(&str, &str), String> {
    let name = arguments[index].as_str();
    let value = arguments
        .get(index + 1)
        .ok_or_else(|| format!("option `{name}` requires a value"))?;
    Ok((name, value))
}
