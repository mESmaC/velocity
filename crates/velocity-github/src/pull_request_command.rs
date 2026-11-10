use serde::de::DeserializeOwned;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PullRequestError {
    #[error("GitHub transport I/O failed: {0}")]
    Process(#[from] std::io::Error),
    #[error("gh failed ({code:?}): {message}")]
    Command { code: Option<i32>, message: String },
    #[error("Invalid GitHub response: {0}")]
    Response(#[from] serde_json::Error),
    #[error("GitHub response is missing {0}")]
    Missing(String),
    #[error("{0}")]
    Invalid(String),
    #[error("Approval record: {0}")]
    Record(#[from] velocity_airc::Error),
}

pub(crate) struct PullRequestCommand {
    directory: PathBuf,
}

impl PullRequestCommand {
    pub fn new(directory: &Path) -> Result<(Self, String), PullRequestError> {
        let directory = std::fs::canonicalize(directory)?;
        // CreateProcess and Git's repository discovery reject long working
        // directories. Read Git metadata in Rust and run gh at the volume root.
        let root = directory
            .ancestors()
            .last()
            .ok_or_else(|| PullRequestError::Invalid("Checkout has no filesystem root".into()))?;
        let repository = origin_url(&directory)?;
        Ok((
            Self {
                directory: root.to_path_buf(),
            },
            repository,
        ))
    }

    pub fn read<T: DeserializeOwned>(&self, arguments: &[&str]) -> Result<T, PullRequestError> {
        self.run(arguments, None)
    }

    /// A command's plain-text output, such as `gh pr diff`.
    pub fn text(&self, arguments: &[&str]) -> Result<String, PullRequestError> {
        let output = self.spawn(arguments, false)?.wait_with_output()?;
        if !output.status.success() {
            return Err(PullRequestError::Command {
                code: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub fn api<T: DeserializeOwned>(
        &self,
        method: &str,
        endpoint: &str,
        body: &Value,
    ) -> Result<T, PullRequestError> {
        self.run(
            &["api", "--method", method, endpoint, "--input", "-"],
            Some(body),
        )
    }

    fn run<T: DeserializeOwned>(
        &self,
        arguments: &[&str],
        body: Option<&Value>,
    ) -> Result<T, PullRequestError> {
        let input = body.map(serde_json::to_vec).transpose()?;
        let mut child = self.spawn(arguments, input.is_some())?;
        if let Some(input) = input {
            let written = child
                .stdin
                .take()
                .ok_or_else(|| PullRequestError::Missing("gh stdin".into()))?
                .write_all(&input);
            if let Err(error) = written {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(PullRequestError::Command {
                code: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    fn spawn(&self, arguments: &[&str], has_input: bool) -> Result<Child, PullRequestError> {
        let mut command = Command::new("gh");
        command
            .current_dir(&self.directory)
            .args(arguments)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_PAGER", "cat")
            .stdin(if has_input {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hide_console(&mut command);
        Ok(command.spawn()?)
    }
}

fn origin_url(directory: &Path) -> Result<String, PullRequestError> {
    let repository = gix::discover(directory).map_err(|error| {
        PullRequestError::Invalid(format!("Could not open Git checkout: {error}"))
    })?;
    let remote = repository
        .find_remote("origin")
        .map_err(|error| PullRequestError::Invalid(format!("Could not read origin: {error}")))?;
    let mut url = remote
        .url(gix::remote::Direction::Fetch)
        .cloned()
        .ok_or_else(|| PullRequestError::Invalid("Origin has no fetch URL".into()))?;
    // gh uses its own credential store. Git remote userinfo must never become
    // process arguments, even when the checkout's origin embeds credentials.
    url.set_user(None);
    url.set_password(None);
    // gh accepts SSH URIs, but not a stripped SCP-style host:path argument.
    if url.scheme == gix::url::Scheme::Ssh && !url.path.starts_with(b"/") {
        url.path = format!("/{}", url.path).into();
    }
    Ok(url
        .with_request_alternate_form(false)
        .to_bstring()
        .to_string())
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
