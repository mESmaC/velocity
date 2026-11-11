mod discovery;
mod runner;

use std::{
    error::Error,
    path::Path,
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    match start() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("stress: {error}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<bool, Box<dyn Error>> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    if arguments.is_empty() {
        return Err("Usage: cargo run -p stress -- --all | --list | <script name>...".into());
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("Missing workspace root")?;
    let mut entries = discovery::discover(root)?;
    if entries.is_empty() {
        return Err("No stress evidence discovered".into());
    }
    // Named scripts run alone, without the workspace tests and UI build that `--all`
    // adds, so checking one script does not hold the build queue for the whole suite.
    let named = arguments != ["--all"] && arguments != ["--list"];
    if named {
        entries = discovery::select(root, entries, &arguments)?;
    }
    if arguments == ["--list"] {
        for entry in entries {
            println!("{}", entry.strip_prefix(root)?.display());
        }
        return Ok(true);
    }
    // The queue owner exports its PID to descendants. Direct invocations also
    // enter the queue; children must inherit ownership to avoid self-deadlock.
    if std::env::var_os("VELOCITY_BUILD_QUEUE_OWNER").is_none() {
        return Ok(Command::new("pwsh")
            .args(["-NoProfile", "-File"])
            .arg(root.join("scripts/build-queue.ps1"))
            .arg(std::env::current_exe()?)
            .args(&arguments)
            .status()?
            .success());
    }
    Ok(runner::run(root, &entries, !named))
}
