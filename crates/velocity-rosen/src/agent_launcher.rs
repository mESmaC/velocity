//! The release app uses the Windows GUI subsystem. This console entry point makes
//! shells wait and supplies the app with the terminal's standard handles.

use std::io;
use std::process::{Command, ExitStatus, Stdio};

fn main() {
    let code = match launch() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!("Could not launch the Velocity agent command: {error}");
            1
        }
    };
    std::process::exit(code);
}

fn launch() -> io::Result<ExitStatus> {
    let executable = std::env::current_exe()?;
    let folder = executable
        .parent()
        .ok_or_else(|| io::Error::other("agent command has no parent folder"))?;
    let app = std::fs::read_to_string(folder.join("velocity-program"))?;
    Command::new(app)
        .arg("--velocity-agent-command")
        .arg(executable)
        .args(std::env::args_os().skip(1))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
}
