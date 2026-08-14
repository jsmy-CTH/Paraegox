use std::ffi::OsStr;
use std::process::ExitCode;

const HELP: &str = "Paraegox — distributed embodied-intelligence Agent OS\n\n\
Usage:\n  paraegox --help\n  paraegox --version\n\n\
Status:\n  Engineering baseline only; no runtime capability is implemented yet.";

fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments.next();

    if arguments.next().is_some() {
        eprintln!("error: expected at most one argument\n\n{HELP}");
        return ExitCode::from(2);
    }

    match first.as_deref().and_then(OsStr::to_str) {
        None | Some("--help" | "-h") => {
            println!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("--version" | "-V") => {
            println!("paraegox {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some(argument) => {
            eprintln!("error: unknown argument `{argument}`\n\n{HELP}");
            ExitCode::from(2)
        }
    }
}
