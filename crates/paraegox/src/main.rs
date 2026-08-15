use std::error::Error;
use std::io::{self, Write};
use std::process::ExitCode;
use std::str::FromStr;
use std::time::Duration;

use paraegox_kernel::{NodeId, RuntimeHostId};
use paraegox_node::{Node, NodeIdentity, probe_node};
use paraegox_runtime::RuntimeHostIdentity;

const DEFAULT_ENDPOINT: &str = "tcp/127.0.0.1:7447";
const DEFAULT_PROBE_TIMEOUT_MS: u64 = 2_000;

const HELP: &str = "Paraegox — distributed embodied-intelligence Agent OS

Usage:
  paraegox --help
  paraegox --version
  paraegox node run --node-id <id> [--listen <loopback-tcp-endpoint>]
  paraegox node probe --target <id> [--connect <loopback-tcp-endpoint>] [--timeout-ms <ms>]

Current capability:
  One addressable Node can run RuntimeHost and FabricService; an external process can probe it.

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
        }) => finish(run_node(node_id, listen_endpoint).await),
        Ok(Command::Probe {
            target,
            connect_endpoint,
            timeout,
        }) => finish(run_probe(target, connect_endpoint, timeout).await),
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
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let identity = NodeIdentity::new(node_id);
    let runtime_host_id = RuntimeHostId::new(format!("{}:runtime-0", identity.node_id))?;
    let runtime_identity = RuntimeHostIdentity::new(runtime_host_id);
    let mut node = Node::new(identity, runtime_identity, listen_endpoint)?;

    node.start().await?;
    println!("{}", serde_json::to_string(&node.status()?)?);
    io::stdout().flush()?;

    if let Err(error) = tokio::signal::ctrl_c().await {
        let stop_result = node.stop().await;
        stop_result?;
        return Err(error.into());
    }

    node.stop().await?;
    Ok(())
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

enum Command {
    Help,
    Version,
    Run {
        node_id: NodeId,
        listen_endpoint: String,
    },
    Probe {
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
        [argument, ..] => Err(format!("unknown argument or command `{argument}`")),
    }
}

fn parse_run(arguments: &[String]) -> Result<Command, String> {
    let mut node_id = None;
    let mut listen_endpoint = DEFAULT_ENDPOINT.to_owned();
    let mut listen_was_set = false;
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
            "--node-id" | "--listen" => return Err(format!("duplicate option `{name}`")),
            _ => return Err(format!("unknown node run option `{name}`")),
        }
        index += 2;
    }

    let node_id = node_id.ok_or_else(|| "node run requires --node-id <id>".to_owned())?;
    Ok(Command::Run {
        node_id,
        listen_endpoint,
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
                timeout_ms = value
                    .parse::<u64>()
                    .map_err(|_| "--timeout-ms must be an integer".to_owned())?;
                if !(100..=60_000).contains(&timeout_ms) {
                    return Err("--timeout-ms must be between 100 and 60000".to_owned());
                }
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

fn option_pair(arguments: &[String], index: usize) -> Result<(&str, &str), String> {
    let name = arguments[index].as_str();
    let value = arguments
        .get(index + 1)
        .ok_or_else(|| format!("option `{name}` requires a value"))?;
    Ok((name, value))
}
