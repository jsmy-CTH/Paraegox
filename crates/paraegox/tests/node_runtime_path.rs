use std::io;
use std::net::TcpListener;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

const NODE_ID: &str = "integration-node";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);

struct NodeProcess {
    child: Child,
}

impl NodeProcess {
    fn spawn(endpoint: &str) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_paraegox"))
            .args(["node", "run", "--node-id", NODE_ID, "--listen", endpoint])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Node process should start");
        Self { child }
    }

    fn stop_with_sigint(mut self) {
        let signal_status = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()
            .expect("SIGINT command should start");
        assert!(signal_status.success(), "SIGINT should be delivered");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self
                .child
                .try_wait()
                .expect("Node status should be readable")
            {
                Some(status) => {
                    assert!(status.success(), "Node should exit cleanly after SIGINT");
                    return;
                }
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                None => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!("Node did not exit after SIGINT");
                }
            }
        }
    }
}

impl Drop for NodeProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn external_probe_observes_runtime_and_restart_identity() {
    let first_endpoint = unused_loopback_endpoint().expect("first test endpoint");
    let first_node = NodeProcess::spawn(&first_endpoint);
    let first_status = wait_for_probe(&first_endpoint);

    assert_eq!(first_status["node"]["node_id"], NODE_ID);
    assert_eq!(first_status["runtime"]["state"], "ready");
    assert_eq!(first_status["fabric"]["ready"], true);
    first_node.stop_with_sigint();
    assert!(!probe(&first_endpoint).status.success());

    let second_node = NodeProcess::spawn(&first_endpoint);
    let second_status = wait_for_probe(&first_endpoint);

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
    output_with_timeout(command, Duration::from_secs(2))
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
