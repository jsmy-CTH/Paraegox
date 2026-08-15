use std::io::{self, BufRead, BufReader};
use std::net::TcpListener;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use paraegox_kernel::{NodeId, RuntimeHostId};
use paraegox_node::{Node, NodeIdentity, NodeRuntimeStatus};
use paraegox_runtime::RuntimeHostIdentity;
use serde_json::Value;

const NODE_ID: &str = "integration-node";
const PROCESS_NODE_A_ID: &str = "process-node-a";
const PROCESS_NODE_B_ID: &str = "process-node-b";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const NODE_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(5);

struct NodeProcess {
    child: Child,
    stdout_lines: mpsc::Receiver<String>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr: Arc<Mutex<String>>,
    stderr_reader: Option<JoinHandle<()>>,
}

impl NodeProcess {
    fn spawn(node_id: &str, endpoint: &str) -> Self {
        Self::spawn_with_arguments(["node", "run", "--node-id", node_id, "--listen", endpoint])
    }

    fn spawn_with_peer(
        node_id: &str,
        listen_endpoint: &str,
        connect_endpoint: &str,
        peer_id: &str,
    ) -> Self {
        Self::spawn_with_arguments([
            "node",
            "run",
            "--node-id",
            node_id,
            "--listen",
            listen_endpoint,
            "--connect",
            connect_endpoint,
            "--probe-peer",
            peer_id,
            "--timeout-ms",
            "2000",
        ])
    }

    fn spawn_with_arguments<const N: usize>(arguments: [&str; N]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_paraegox"))
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Node process should start");

        let stdout = child.stdout.take().expect("Node stdout should be piped");
        let (stdout_sender, stdout_lines) = mpsc::channel();
        let stdout_reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if stdout_sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
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

    fn next_json_line(&mut self, timeout: Duration) -> Value {
        match self.stdout_lines.recv_timeout(timeout) {
            Ok(line) => serde_json::from_str(&line)
                .unwrap_or_else(|error| panic!("Node stdout was not JSON: {error}; line: {line}")),
            Err(error) => {
                let status = self
                    .child
                    .try_wait()
                    .expect("Node process status should be readable");
                panic!(
                    "Node did not emit expected JSON before the deadline ({error}); status: {status:?}; stderr: {}",
                    self.stderr_snapshot()
                );
            }
        }
    }

    fn assert_running(&mut self) {
        let status = self
            .child
            .try_wait()
            .expect("Node process status should be readable");
        assert!(
            status.is_none(),
            "Node exited unexpectedly with {status:?}; stderr: {}",
            self.stderr_snapshot()
        );
    }

    fn stop_with_sigint(&mut self) {
        self.assert_running();
        let signal_status = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()
            .expect("SIGINT command should start");
        assert!(signal_status.success(), "SIGINT should be delivered");

        let status = self.wait_for_exit(SHUTDOWN_TIMEOUT);
        assert!(
            status.success(),
            "Node should exit cleanly after SIGINT; stderr: {}",
            self.stderr_snapshot()
        );
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        let status = loop {
            match self
                .child
                .try_wait()
                .expect("Node status should be readable")
            {
                Some(status) => break status,
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                None => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    self.join_readers();
                    panic!(
                        "Node did not exit before the deadline; stderr: {}",
                        self.stderr_snapshot()
                    );
                }
            }
        };
        self.join_readers();
        status
    }

    fn stderr_snapshot(&self) -> String {
        self.stderr.lock().expect("stderr capture lock").clone()
    }

    fn join_readers(&mut self) {
        if let Some(reader) = self.stdout_reader.take() {
            reader.join().expect("stdout reader should not panic");
        }
        if let Some(reader) = self.stderr_reader.take() {
            reader.join().expect("stderr reader should not panic");
        }
    }
}

impl Drop for NodeProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        self.join_readers();
    }
}

#[test]
fn external_probe_observes_runtime_and_restart_identity() {
    let first_endpoint = unused_loopback_endpoint().expect("first test endpoint");
    let mut first_node = NodeProcess::spawn(NODE_ID, &first_endpoint);
    let first_local_status = first_node.next_json_line(STARTUP_TIMEOUT);
    let first_status = wait_for_probe(&first_endpoint);

    assert_eq!(first_status["node"]["node_id"], NODE_ID);
    assert_eq!(first_status["runtime"]["state"], "ready");
    assert_eq!(first_status["fabric"]["ready"], true);
    assert_eq!(
        first_status, first_local_status,
        "the external probe must observe the running Node's exact status"
    );
    first_node.stop_with_sigint();
    assert!(!probe(&first_endpoint).status.success());

    let mut second_node = NodeProcess::spawn(NODE_ID, &first_endpoint);
    let second_local_status = second_node.next_json_line(STARTUP_TIMEOUT);
    let second_status = wait_for_probe(&first_endpoint);

    assert_eq!(
        second_status, second_local_status,
        "the external probe must observe the restarted Node's exact status"
    );
    assert_ne!(
        first_status["node"]["incarnation"], second_status["node"]["incarnation"],
        "a restarted Node must have a new incarnation"
    );
    assert_ne!(
        first_status["runtime"]["identity"]["epoch"], second_status["runtime"]["identity"]["epoch"],
        "a restarted RuntimeHost must have a new epoch"
    );
    second_node.stop_with_sigint();
}

#[test]
fn two_node_runs_use_node_bs_session_to_observe_node_a() {
    let (node_a_endpoint, node_b_endpoint) =
        two_unused_loopback_endpoints().expect("two test endpoints");
    let mut node_a = NodeProcess::spawn(PROCESS_NODE_A_ID, &node_a_endpoint);
    let node_a_status = node_a.next_json_line(STARTUP_TIMEOUT);
    node_a.assert_running();

    let mut node_b = NodeProcess::spawn_with_peer(
        PROCESS_NODE_B_ID,
        &node_b_endpoint,
        &node_a_endpoint,
        PROCESS_NODE_A_ID,
    );
    let node_b_status = node_b.next_json_line(STARTUP_TIMEOUT);
    let peer_observation = node_b.next_json_line(STARTUP_TIMEOUT);

    assert_eq!(node_a_status["node"]["node_id"], PROCESS_NODE_A_ID);
    assert_eq!(node_b_status["node"]["node_id"], PROCESS_NODE_B_ID);
    assert_eq!(node_a_status["runtime"]["state"], "ready");
    assert_eq!(node_b_status["runtime"]["state"], "ready");
    assert_eq!(
        peer_observation["peer"], node_a_status,
        "Node B must observe Node A's exact incarnation, RuntimeHost epoch, and readiness"
    );
    node_a.assert_running();
    node_b.assert_running();

    node_b.stop_with_sigint();
    node_a.stop_with_sigint();

    let mut missing_peer = NodeProcess::spawn_with_peer(
        PROCESS_NODE_B_ID,
        &node_b_endpoint,
        &node_a_endpoint,
        PROCESS_NODE_A_ID,
    );
    let missing_peer_local_status = missing_peer.next_json_line(STARTUP_TIMEOUT);
    assert_eq!(
        missing_peer_local_status["node"]["node_id"],
        PROCESS_NODE_B_ID
    );
    let missing_peer_status = missing_peer.wait_for_exit(STARTUP_TIMEOUT);
    assert!(
        !missing_peer_status.success(),
        "Node B must fail when its configured peer is unavailable"
    );
    assert!(
        missing_peer.stderr_snapshot().contains("error:"),
        "peer failure must be reported on stderr"
    );
    assert_endpoint_reusable(&node_b_endpoint);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn long_lived_node_session_recovers_after_peer_restart() {
    let (node_a_endpoint, node_b_endpoint) =
        two_unused_loopback_endpoints().expect("two test endpoints");
    let node_a_id = NodeId::from_str("session-node-a").expect("valid Node A id");
    let node_b_id = NodeId::from_str("session-node-b").expect("valid Node B id");
    let mut node_a = test_node(node_a_id.clone(), &node_a_endpoint, None);
    let mut node_b = test_node(
        node_b_id.clone(),
        &node_b_endpoint,
        Some(node_a_endpoint.clone()),
    );

    start_node(&mut node_a).await;
    start_node(&mut node_b).await;
    let self_probe = node_b
        .probe_peer(&node_b_id, Duration::from_millis(100))
        .await;
    assert!(
        self_probe.is_err(),
        "a Node must not produce peer evidence by querying itself"
    );
    let first_observation = wait_for_peer(&node_b, &node_a_id, STARTUP_TIMEOUT).await;
    assert_eq!(
        first_observation,
        node_a.status().expect("Node A should be ready"),
        "Node B must observe Node A's current status through its own session"
    );

    stop_node(&mut node_a).await;
    let stopped_probe = tokio::time::timeout(
        Duration::from_secs(1),
        node_b.probe_peer(&node_a_id, Duration::from_millis(400)),
    )
    .await
    .expect("probing a stopped peer must honor the wall-clock deadline");
    assert!(
        stopped_probe.is_err(),
        "Node B must not report stopped Node A as ready"
    );

    let mut restarted_node_a = test_node(node_a_id.clone(), &node_a_endpoint, None);
    start_node(&mut restarted_node_a).await;
    let restarted_observation = wait_for_peer(&node_b, &node_a_id, STARTUP_TIMEOUT).await;
    assert_eq!(
        restarted_observation,
        restarted_node_a
            .status()
            .expect("restarted Node A should be ready"),
        "Node B's original session must observe restarted Node A"
    );
    assert_ne!(
        first_observation.node.incarnation, restarted_observation.node.incarnation,
        "restarted Node A must have a new incarnation"
    );
    assert_ne!(
        first_observation.runtime.identity.epoch, restarted_observation.runtime.identity.epoch,
        "restarted Node A must have a new RuntimeHost epoch"
    );

    stop_node(&mut node_b).await;
    stop_node(&mut restarted_node_a).await;
}

fn test_node(node_id: NodeId, listen_endpoint: &str, connect_endpoint: Option<String>) -> Node {
    let runtime_host_id =
        RuntimeHostId::new(format!("{node_id}:runtime-0")).expect("valid RuntimeHost id");
    Node::new(
        NodeIdentity::new(node_id),
        RuntimeHostIdentity::new(runtime_host_id),
        listen_endpoint,
        connect_endpoint,
    )
    .expect("valid Node configuration")
}

async fn start_node(node: &mut Node) {
    tokio::time::timeout(NODE_LIFECYCLE_TIMEOUT, node.start())
        .await
        .expect("Node start must honor the wall-clock deadline")
        .expect("Node should start");
}

async fn stop_node(node: &mut Node) {
    tokio::time::timeout(NODE_LIFECYCLE_TIMEOUT, node.stop())
        .await
        .expect("Node stop must honor the wall-clock deadline")
        .expect("Node should stop");
}

async fn wait_for_peer(node: &Node, target: &NodeId, timeout: Duration) -> NodeRuntimeStatus {
    let absolute_deadline = Instant::now() + timeout;
    let mut last_error = "no query attempted".to_owned();

    loop {
        let remaining = absolute_deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "Node B did not observe Node A before the absolute deadline: {last_error}"
        );
        let query_timeout = remaining.min(Duration::from_millis(500));
        match tokio::time::timeout(
            query_timeout.saturating_add(Duration::from_millis(100)),
            node.probe_peer(target, query_timeout),
        )
        .await
        {
            Ok(Ok(status)) => return status,
            Ok(Err(error)) => last_error = error.to_string(),
            Err(_) => last_error = "peer query exceeded its independent deadline".to_owned(),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn wait_for_probe(endpoint: &str) -> Value {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let output = probe(endpoint);
        if output.status.success() {
            return serde_json::from_slice(&output.stdout).expect("probe should return JSON");
        }
        assert!(
            Instant::now() < deadline,
            "Node did not become probeable: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn probe(endpoint: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_paraegox"));
    command.args([
        "node",
        "probe",
        "--target",
        NODE_ID,
        "--connect",
        endpoint,
        "--timeout-ms",
        "300",
    ]);
    output_with_timeout(command, Duration::from_secs(5))
}

fn output_with_timeout(mut command: Command, timeout: Duration) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("probe process should start");
    let deadline = Instant::now() + timeout;

    loop {
        if child
            .try_wait()
            .expect("probe process status should be readable")
            .is_some()
        {
            return child
                .wait_with_output()
                .expect("probe output should be readable");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .expect("timed-out probe output should be readable");
            panic!(
                "probe exceeded its independent test deadline: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn unused_loopback_endpoint() -> io::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(format!("tcp/127.0.0.1:{port}"))
}

fn two_unused_loopback_endpoints() -> io::Result<(String, String)> {
    let first_listener = TcpListener::bind("127.0.0.1:0")?;
    let second_listener = TcpListener::bind("127.0.0.1:0")?;
    let first_port = first_listener.local_addr()?.port();
    let second_port = second_listener.local_addr()?.port();
    drop((first_listener, second_listener));
    Ok((
        format!("tcp/127.0.0.1:{first_port}"),
        format!("tcp/127.0.0.1:{second_port}"),
    ))
}

fn assert_endpoint_reusable(endpoint: &str) {
    let address = endpoint
        .strip_prefix("tcp/")
        .expect("test endpoint should use tcp/");
    TcpListener::bind(address).expect("stopped Node must release its listen endpoint");
}
