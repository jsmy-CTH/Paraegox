use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn run_paraegox(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_paraegox"))
        .args(arguments)
        .output()
        .expect("the paraegox binary should start")
}

fn run_paraegox_with_timeout(
    arguments: &[&str],
    removed_environment: &str,
    timeout: Duration,
) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_paraegox"))
        .args(arguments)
        .env_remove(removed_environment)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the paraegox binary should start");
    let deadline = Instant::now() + timeout;

    loop {
        if child
            .try_wait()
            .expect("the paraegox process status should be readable")
            .is_some()
        {
            return child
                .wait_with_output()
                .expect("the paraegox process output should be readable");
        }
        if Instant::now() >= deadline {
            child
                .kill()
                .expect("a hung paraegox process should be killable");
            let output = child
                .wait_with_output()
                .expect("the killed paraegox process output should be readable");
            panic!(
                "paraegox did not exit before the deadline; stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn help_reports_the_product_and_current_boundary() {
    let output = run_paraegox(&["--help"]);

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help should be UTF-8");
    assert!(stdout.contains("distributed embodied-intelligence Agent OS"));
    assert!(stdout.contains("node run"));
    assert!(stdout.contains("node probe"));
    assert!(!stdout.contains("AgentService"));
}

#[test]
fn version_comes_from_the_package() {
    let output = run_paraegox(&["--version"]);

    assert!(output.status.success());
    assert_eq!(output.stdout, b"paraegox 0.1.0\n");
}

#[test]
fn unknown_arguments_fail_as_cli_errors() {
    let output = run_paraegox(&["chat"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("error should be UTF-8");
    assert!(stderr.contains("unknown argument or command `chat`"));
}

#[test]
fn deepseek_provider_without_api_key_fails_before_node_start() {
    let output = run_paraegox_with_timeout(
        &[
            "node",
            "run",
            "--node-id",
            "deepseek-missing-key",
            "--deck",
            "builtin-agent",
            "--provider",
            "deepseek-v4-flash",
        ],
        "DEEPSEEK_API_KEY",
        Duration::from_secs(10),
    );

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("error should be UTF-8");
    assert!(stderr.contains("DEEPSEEK_API_KEY is not set"));
    assert!(!stderr.contains("Authorization"));
    assert!(!stderr.contains("Bearer"));
}
