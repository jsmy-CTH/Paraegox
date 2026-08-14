use std::process::{Command, Output};

fn run_paraegox(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_paraegox"))
        .args(arguments)
        .output()
        .expect("the paraegox binary should start")
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
