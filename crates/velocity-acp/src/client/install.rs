//! Running an agent's install (F15 Install, #221): each step of `install::install_steps` in
//! turn, with no console window. Dropping the future stops the running step and everything
//! it started.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::program_path::find_program;
use super::{ClientError, process};
use crate::install::InstallStep;

/// How many of a failed step's last output lines its error carries.
const OUTPUT_LINES_KEPT: usize = 4;

/// Runs `steps` in `directory` and returns when the last one finished, one failed, or
/// `deadline` passed.
pub async fn install_agent(
    steps: &[InstallStep],
    directory: &Path,
    deadline: Duration,
) -> Result<(), ClientError> {
    let install = async {
        for step in steps {
            run_step(step, directory).await?;
        }
        Ok(())
    };
    tokio::time::timeout(deadline, install)
        .await
        .map_err(|_| ClientError::TimedOut(deadline))?
}

async fn run_step(step: &InstallStep, directory: &Path) -> Result<(), ClientError> {
    let mut command = Command::new(find_program(OsStr::new(&step.program)));
    command
        .args(&step.args)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = process::spawn_owned_command(command).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            ClientError::Install(missing_program(&step.program))
        } else {
            ClientError::Spawn {
                program: step.program.clone(),
                source,
            }
        }
    })?;
    let stdout = child.stdout().take();
    let stderr = child.stderr().take();
    let (stdout, stderr) = tokio::join!(read_all(stdout), read_all(stderr));
    let status = child.wait().await?;
    if step.succeeded(status.code()) {
        return Ok(());
    }
    let output = last_lines(&format!("{stdout}\n{stderr}"));
    let detail = if output.is_empty() {
        String::new()
    } else {
        format!(": {output}")
    };
    Err(ClientError::Install(format!(
        "`{}` exited with {status}{detail}",
        step.command_line()
    )))
}

/// What to do when the package manager itself is missing.
fn missing_program(program: &str) -> String {
    match program {
        "npm" => "npm isn't installed. Install Node.js 22 or later, then Install again.".into(),
        "winget" => "winget isn't installed. Install App Installer from the Microsoft Store, then Install again.".into(),
        other => format!("{other} isn't installed"),
    }
}

async fn read_all(pipe: Option<impl AsyncRead + Unpin>) -> String {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        // A read error only loses output for the error message; the exit status decides.
        let _ = pipe.read_to_end(&mut bytes).await;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The last few non-empty lines, joined, without the progress spinners installers draw.
fn last_lines(output: &str) -> String {
    let lines: Vec<&str> = output
        .lines()
        .map(str::trim)
        .filter(|line| line.chars().any(char::is_alphanumeric))
        .collect();
    let start = lines.len().saturating_sub(OUTPUT_LINES_KEPT);
    lines[start..].join(" · ")
}
