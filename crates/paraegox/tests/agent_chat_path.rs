use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use paraegox_agent::BUILTIN_AGENT_DEFINITION;
use serde_json::Value;

const NODE_ID: &str = "agent-chat-node";
const FIRST_INPUT: &str = "remember project alpha";
const SECOND_INPUT: &str = "what did I ask before?";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const CHAT_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

struct RunningNode {
    child: Child,
    stdout_lines: mpsc::Receiver<String>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr: Arc<Mutex<String>>,
    stderr_reader: Option<JoinHandle<()>>,
}

impl RunningNode {
    fn spawn(endpoint: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_paraegox"))
            .args([
                "node",
                "run",
                "--node-id",
                NODE_ID,
                "--listen",
                endpoint,
                "--deck",
                "builtin-agent",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("built-in Agent Node process should start");

        let stdout = child.stdout.take().expect("Node stdout should be piped");
        let (stdout_sender, stdout_lines) = mpsc::channel();
        let stdout_reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if stdout_sender.send(line).is_err() {
                    break;
                }
            }
        });

        let stderr = Arc::new(Mutex::new(String::new()));
        let stderr_capture = Arc::clone(&stderr);
        let process_stderr = child.stderr.take().expect("Node stderr should be piped");
        let stderr_reader = thread::spawn(move || {
            for line in BufReader::new(process_stderr).lines() {
                let Ok(line) = line else {
                    break;
                };
                let mut capture = stderr_capture.lock().expect("stderr capture lock");
                if !capture.is_empty() {
                    capture.push('\n');
                }
                capture.push_str(&line);
            }
        });

        Self {
            child,
            stdout_lines,
            stdout_reader: Some(stdout_reader),
            stderr,
            stderr_reader: Some(stderr_reader),
        }
    }

    fn status(&mut self) -> Value {
        let line = self
            .stdout_lines
            .recv_timeout(STARTUP_TIMEOUT)
            .unwrap_or_else(|error| {
                let status = self.child.try_wait().expect("Node status should be readable");
                panic!(
                    "Node did not publish status before its deadline ({error}); status: {status:?}; stderr: {}",
                    self.stderr_snapshot()
                );
            });
        serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("Node status was not JSON: {error}; line: {line}"))
    }

    fn stop(&mut self) {
        assert!(
            self.child
                .try_wait()
                .expect("Node status should be readable")
                .is_none(),
            "Node exited before shutdown; stderr: {}",
            self.stderr_snapshot()
        );
        let signal_status = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()
            .expect("SIGINT command should start");
        assert!(signal_status.success(), "SIGINT should be delivered");

        let status = wait_for_exit(&mut self.child, SHUTDOWN_TIMEOUT);
        self.join_readers();
        assert!(
            status.success(),
            "Node should stop cleanly; stderr: {}",
            self.stderr_snapshot()
        );
    }

    fn stderr_snapshot(&self) -> String {
        self.stderr.lock().expect("stderr capture lock").clone()
    }

    fn join_readers(&mut self) {
        if let Some(reader) = self.stdout_reader.take() {
            reader.join().expect("Node stdout reader should not panic");
        }
        if let Some(reader) = self.stderr_reader.take() {
            reader.join().expect("Node stderr reader should not panic");
        }
    }
}

impl Drop for RunningNode {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.join_readers();
    }
}

#[test]
fn builtin_deck_runs_real_card_and_tui_preserves_server_history() {
    let endpoint = unused_loopback_endpoint();
    let mut node = RunningNode::spawn(&endpoint);
    let status = node.status();
    let deck_run = &status["runtime"]["deck_run"];
    let card = &deck_run["cards"][0];

    assert_eq!(status["node"]["node_id"], NODE_ID);
    assert_eq!(status["runtime"]["state"], "ready");
    assert_eq!(deck_run["deck_key"], "builtin-agent");
    assert_eq!(deck_run["generation"], 1);
    assert_eq!(deck_run["state"], "ready");
    let lock_digest = deck_run["lock_digest"]
        .as_str()
        .expect("DeckLock digest should be a string");
    assert_eq!(lock_digest.len(), 64);
    assert!(lock_digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_eq!(card["key"], "agent");
    assert_eq!(card["definition"], BUILTIN_AGENT_DEFINITION);
    assert_eq!(card["generation"], 1);
    assert_eq!(card["state"], "ready");

    let mut tui = Command::new(env!("CARGO_BIN_EXE_paraegox"))
        .args([
            "tui",
            "--target",
            NODE_ID,
            "--connect",
            &endpoint,
            "--timeout-ms",
            "3000",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("TUI process should start");
    let mut stdin = tui.stdin.take().expect("TUI stdin should be piped");
    writeln!(stdin, "{FIRST_INPUT}").expect("first input should be written");
    writeln!(stdin, "{SECOND_INPUT}").expect("second input should be written");
    writeln!(stdin, "/quit").expect("quit command should be written");
    drop(stdin);

    let output = wait_for_output(tui, CHAT_TIMEOUT);
    assert!(
        output.status.success(),
        "TUI should exit cleanly on /quit; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("TUI stdout should be UTF-8");
    assert_eq!(
        stdout,
        format!(
            "agent> current: {FIRST_INPUT}\nagent> previous: {FIRST_INPUT}; current: {SECOND_INPUT}\n"
        ),
        "the second final must quote the exact prior input held by AgentService"
    );

    node.stop();
}

fn unused_loopback_endpoint() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback port should bind");
    let address = listener
        .local_addr()
        .expect("loopback address should be readable");
    drop(listener);
    format!("tcp/{address}")
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("process status should be readable") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("process exceeded its shutdown deadline");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_output(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    loop {
        if child
            .try_wait()
            .expect("TUI process status should be readable")
            .is_some()
        {
            return child
                .wait_with_output()
                .expect("TUI output should be readable");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .expect("timed-out TUI output should be readable");
            panic!(
                "TUI exceeded its independent deadline; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}
