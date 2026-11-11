use serde::de::DeserializeOwned;
use serde_json::Value;
use std::io::Write;
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};
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
    /// Where this client was created, so each `gh` call is counted against its feature.
    caller: &'static Location<'static>,
    /// The checkout's `origin` when the client was created, and when that was: a cached client
    /// is current only while both still hold (#229).
    origin: String,
    created_at: Instant,
}

impl PullRequestCommand {
    #[track_caller]
    pub fn new(directory: &Path) -> Result<(Self, String), PullRequestError> {
        let caller = Location::caller();
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
                caller,
                origin: repository.clone(),
                created_at: Instant::now(),
            },
            repository,
        ))
    }

    /// Whether `directory`'s `origin` is still the one this client was created for, and the
    /// client is younger than `lifetime`. Reading the origin is local Git config, no request.
    pub fn is_current(&self, directory: &Path, lifetime: Duration) -> bool {
        self.created_at.elapsed() < lifetime
            && std::fs::canonicalize(directory)
                .ok()
                .and_then(|directory| origin_url(&directory).ok())
                .is_some_and(|origin| origin == self.origin)
    }

    pub fn read<T: DeserializeOwned>(&self, arguments: &[&str]) -> Result<T, PullRequestError> {
        self.run(arguments, None, None)
    }

    /// A command's plain-text output, such as `gh pr diff`.
    pub fn text(&self, arguments: &[&str]) -> Result<String, PullRequestError> {
        let output = self.spawn(arguments, false, None)?.wait_with_output()?;
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
            None,
        )
    }

    /// A GraphQL request, counted under the response path it reads.
    pub fn graphql<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, PullRequestError> {
        self.run(
            &["api", "--method", "POST", "graphql", "--input", "-"],
            Some(body),
            Some(path),
        )
    }

    fn run<T: DeserializeOwned>(
        &self,
        arguments: &[&str],
        body: Option<&Value>,
        graphql_path: Option<&str>,
    ) -> Result<T, PullRequestError> {
        let input = body.map(serde_json::to_vec).transpose()?;
        let mut child = self.spawn(arguments, input.is_some(), graphql_path)?;
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

    fn spawn(
        &self,
        arguments: &[&str],
        has_input: bool,
        graphql_path: Option<&str>,
    ) -> Result<Child, PullRequestError> {
        crate::request_counter::record(
            self.caller,
            crate::request_counter::operation(arguments, graphql_path),
        );
        let mut command = crate::command::hidden_command("gh");
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
