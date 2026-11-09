use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::{Deserialize, Serialize};

use crate::issue_types::content_hash;
use crate::scout_directory::ReproductionDirectory;
use crate::scout_issues::ScoutIssueError;

/// Backend-only reproduction command. It is deliberately not an IPC command:
/// the frontend must never be able to submit a shell command (§3).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Reproduction {
    pub program: PathBuf,
    #[serde(default)]
    pub arguments: Vec<String>,
    pub script: PathBuf,
    #[serde(default)]
    pub script_arguments: Vec<String>,
}

pub(crate) struct FailureEvidence {
    pub attachment: String,
}

impl Reproduction {
    pub(crate) fn run(&self, directory: &Path) -> Result<FailureEvidence, ScoutIssueError> {
        let directory = std::fs::canonicalize(directory)?;
        let script = std::fs::canonicalize(directory.join(&self.script))?;
        let source = read_source(&script)?;
        let working_directory = ReproductionDirectory::new(&directory)?;
        let mut command = Command::new(&self.program);
        command
            .current_dir(working_directory.path())
            .args(&self.arguments)
            .arg(working_directory.script_path(&directory, &script))
            .args(&self.script_arguments)
            .stdin(Stdio::null());
        hide_console(&mut command);
        let output = capture(&mut command)?;
        if output.status.success() {
            return Err(ScoutIssueError::NotReproduced);
        }
        if read_source(&script)? != source {
            return Err(ScoutIssueError::Invalid(
                "Evidence script changed while it ran; reproduce again".into(),
            ));
        }
        let stdout = String::from_utf8(output.stdout).map_err(invalid_utf8)?;
        let stderr = String::from_utf8(output.stderr).map_err(invalid_utf8)?;
        self.attachment(&source, &output.status.to_string(), &stdout, &stderr)
    }

    fn attachment(
        &self,
        source: &str,
        exit_status: &str,
        stdout: &str,
        stderr: &str,
    ) -> Result<FailureEvidence, ScoutIssueError> {
        let command = serde_json::to_string(self)?;
        let source_hash = content_hash(source.as_bytes())?;
        let mut attachment = format!(
            "## Failure evidence\n\nReproduction arguments (JSON; run without a shell):\n\n{}\n\nScript: {} UTF-8 bytes. SHA-256: {source_hash}.\n\nStatus: {exit_status}.\n\nOutput: {} stdout bytes, {} stderr bytes.\n\n",
            fenced(&command),
            source.len(),
            stdout.len(),
            stderr.len()
        );
        attachment.push_str(&format!(
            "### Reproducer\n\n{}\n\n### Standard output\n\n{}\n\n### Standard error\n\n{}",
            fenced(source),
            fenced(stdout),
            fenced(stderr)
        ));
        if attachment.len() > 56_000 {
            return Err(ScoutIssueError::Invalid(
                "Evidence exceeds GitHub's issue body budget; use a smaller reproduction".into(),
            ));
        }
        Ok(FailureEvidence { attachment })
    }
}

fn read_source(path: &Path) -> Result<String, ScoutIssueError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(48_001)
        .read_to_end(&mut bytes)?;
    let source = String::from_utf8(bytes).map_err(invalid_utf8)?;
    if source.trim().is_empty() || source.len() > 48_000 {
        return Err(ScoutIssueError::Invalid(
            "Evidence script must contain 1–48000 UTF-8 bytes".into(),
        ));
    }
    Ok(source)
}

fn capture(command: &mut Command) -> io::Result<Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing reproduction stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("Missing reproduction stderr"))?;
    std::thread::scope(|scope| {
        let stdout = scope.spawn(|| read_output(stdout));
        let stderr = scope.spawn(|| read_output(stderr));
        let status = child.wait()?;
        let stdout = stdout
            .join()
            .map_err(|_| io::Error::other("Could not capture reproduction stdout"))??;
        let stderr = stderr
            .join()
            .map_err(|_| io::Error::other("Could not capture reproduction stderr"))??;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    })
}

fn read_output(mut pipe: impl Read) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    pipe.by_ref().take(56_001).read_to_end(&mut output)?;
    // Drain oversized output without retaining it, so a noisy stress script can
    // finish without either exhausting memory or blocking on a full pipe.
    io::copy(&mut pipe, &mut io::sink())?;
    if output.len() > 56_000 {
        return Err(io::Error::other(
            "Evidence output exceeds the issue body budget",
        ));
    }
    Ok(output)
}

pub(crate) fn fenced(value: &str) -> String {
    // Reproducers can contain Markdown themselves. The delimiter must be longer
    // than any run in the attached bytes so no code escapes its evidence block.
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(3.max(longest + 1));
    let separator = if value.ends_with('\n') { "" } else { "\n" };
    format!("{fence}\n{value}{separator}{fence}")
}

fn invalid_utf8(error: std::string::FromUtf8Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

fn hide_console(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    #[cfg(not(windows))]
    let _ = command;
}
