use serde::de::DeserializeOwned;
use serde_json::Value;
use std::io::{Read, Write};
use std::panic::Location;
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::http_transport::{self, TransportAttempt};

/// Why a `gh` or GitHub API call for an issue, label, status or repository read failed.
#[derive(Debug, Error)]
pub enum GitHubCommandError {
    #[error("GitHub transport I/O failed: {0}")]
    Process(#[from] std::io::Error),
    #[error("gh failed ({code:?}): {message}")]
    Command { code: Option<i32>, message: String },
    #[error("gh did not answer within {0} seconds and was stopped")]
    TimedOut(u64),
    #[error("Invalid GitHub response: {0}")]
    Response(#[from] serde_json::Error),
    #[error("GitHub response is missing {0}")]
    Missing(String),
    #[error("{0}")]
    Invalid(String),
}

impl GitHubCommandError {
    /// Whether this failure came from Velocity's local environment or `gh` invocation
    /// rather than GitHub's API, network reachability, or caller input validation (#581).
    pub fn is_local_tool_failure(&self) -> bool {
        match self {
            Self::Process(_) | Self::Response(_) => true,
            Self::TimedOut(_) => false,
            Self::Missing(what) => !what.starts_with('/') && !what.contains("pagination"),
            Self::Command { code, message } => !is_github_command_failure(*code, message),
            Self::Invalid(message) => is_local_invalid_message(message),
        }
    }
}

fn is_github_command_failure(code: Option<i32>, message: &str) -> bool {
    if code.is_some_and(|status| (400..=599).contains(&status)) && message.starts_with("HTTP ") {
        return true;
    }
    let lower = message.to_ascii_lowercase();
    lower.contains("http 4")
        || lower.contains("http 5")
        || lower.contains("graphql:")
        || lower.contains("(graphql)")
        || lower.contains("error connecting to")
        || lower.contains("dial tcp")
        || lower.contains("no such host")
        || lower.contains("tls handshake")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("i/o timeout")
        || lower.contains("network is unreachable")
        || lower.contains("unexpected eof")
        || lower.contains("rate limit")
}

fn is_local_invalid_message(message: &str) -> bool {
    message.starts_with("GitHub HTTP worker ")
        || message.starts_with("Could not open Git checkout")
        || message.starts_with("Could not read origin")
        || message == "Origin has no fetch URL"
        || message == "Checkout has no filesystem root"
        || message == "Invalid GitHub repository name"
}

/// How long one `gh` call may run. GitHub answers in seconds; a call still running after
/// this is hung (a stalled connection, a prompt nobody can answer), and whatever waits on
/// it, such as an issue panel refresh or a post-land report, would otherwise wait for good.
const DEADLINE: Duration = Duration::from_secs(120);
/// How often a running `gh` is checked for its exit.
const POLL: Duration = Duration::from_millis(10);

pub(crate) struct GitHubCommand {
    directory: PathBuf,
    /// Where this client was created, so each call is counted against its feature.
    caller: &'static Location<'static>,
    /// The checkout's `origin` when the client was created, and when that was: a cached client
    /// is current only while both still hold (#229).
    origin: String,
    /// Whether this client targets `github.com` and may use the in-process HTTPS transport.
    github_com: bool,
    created_at: Instant,
}

impl GitHubCommand {
    /// A Rosen project can have a GitHub mirror without a local git checkout. Keep gh's
    /// working directory short and pass the validated repository explicitly to its API.
    #[track_caller]
    pub(crate) fn for_repository(repository: &crate::RepositoryName) -> Self {
        Self {
            directory: std::env::temp_dir()
                .ancestors()
                .last()
                .unwrap_or(Path::new("."))
                .to_path_buf(),
            caller: Location::caller(),
            origin: repository.full_name(),
            github_com: true,
            created_at: Instant::now(),
        }
    }

    #[track_caller]
    pub fn new(directory: &Path) -> Result<(Self, String), GitHubCommandError> {
        let caller = Location::caller();
        let directory = std::fs::canonicalize(directory)?;
        // CreateProcess and Git's repository discovery reject long working
        // directories. Read Git metadata in Rust and run gh at the volume root.
        let root = directory
            .ancestors()
            .last()
            .ok_or_else(|| GitHubCommandError::Invalid("Checkout has no filesystem root".into()))?;
        let (repository, github_com) = origin_url(&directory)?;
        Ok((
            Self {
                directory: root.to_path_buf(),
                caller,
                origin: repository.clone(),
                github_com,
                created_at: Instant::now(),
            },
            repository,
        ))
    }

    pub(crate) fn is_github_com(&self) -> bool {
        self.github_com
    }

    /// Whether `directory`'s `origin` is still the one this client was created for, and the
    /// client is younger than `lifetime`. Reading the origin is local Git config, no request.
    pub fn is_current(&self, directory: &Path, lifetime: Duration) -> bool {
        self.created_at.elapsed() < lifetime
            && std::fs::canonicalize(directory)
                .ok()
                .and_then(|directory| origin_url(&directory).ok())
                .is_some_and(|(origin, _)| origin == self.origin)
    }

    pub fn read<T: DeserializeOwned>(&self, arguments: &[&str]) -> Result<T, GitHubCommandError> {
        if self.github_com
            && let Some((endpoint, accept)) = parse_simple_api_get(arguments)
            && let TransportAttempt::Handled(result) =
                http_transport::get(&self.origin, self.caller, endpoint, accept)
        {
            return Ok(serde_json::from_slice(&result?)?);
        }
        self.run(arguments, None, None)
    }

    /// Paginated REST GET via HTTP with per-page ETag caching, falling back to `gh` when
    /// the in-process HTTP token is unavailable.
    pub(crate) fn get_paginated<T: DeserializeOwned>(
        &self,
        endpoint: &str,
    ) -> Result<Vec<T>, GitHubCommandError> {
        if self.github_com
            && let TransportAttempt::Handled(result) =
                http_transport::get_paginated(&self.origin, self.caller, endpoint)
        {
            return result;
        }
        let pages: Vec<Vec<T>> =
            self.run(&["api", "--paginate", "--slurp", endpoint], None, None)?;
        Ok(pages.into_iter().flatten().collect())
    }

    /// A command's plain-text output.
    pub fn text(&self, arguments: &[&str]) -> Result<String, GitHubCommandError> {
        if self.github_com {
            if let Some((endpoint, accept)) = parse_simple_api_get(arguments)
                && let TransportAttempt::Handled(result) =
                    http_transport::get(&self.origin, self.caller, endpoint, accept)
            {
                return Ok(String::from_utf8_lossy(&result?).into_owned());
            }
            if let Some((method, endpoint)) = parse_simple_api_method(arguments)
                && let TransportAttempt::Handled(result) =
                    http_transport::send(&self.origin, self.caller, method, endpoint, None, None)
            {
                return Ok(String::from_utf8_lossy(&result?).into_owned());
            }
        }
        let output = finish(self.spawn(arguments, false, None)?, DEADLINE)?;
        if !output.status.success() {
            return Err(GitHubCommandError::Command {
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
    ) -> Result<T, GitHubCommandError> {
        if self.github_com {
            let encoded = serde_json::to_vec(body)?;
            if let TransportAttempt::Handled(result) = http_transport::send(
                &self.origin,
                self.caller,
                method,
                endpoint,
                Some(&encoded),
                None,
            ) {
                return Ok(serde_json::from_slice(&result?)?);
            }
        }
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
    ) -> Result<T, GitHubCommandError> {
        if self.github_com {
            let encoded = serde_json::to_vec(body)?;
            if let TransportAttempt::Handled(result) = http_transport::send(
                &self.origin,
                self.caller,
                "POST",
                "graphql",
                Some(&encoded),
                Some(path),
            ) {
                return Ok(serde_json::from_slice(&result?)?);
            }
        }
        self.run(
            &["api", "--method", "POST", "graphql", "--input", "-"],
            Some(body),
            Some(path),
        )
    }

    /// A REST request's answer as GitHub sent it, for an endpoint that may answer with no
    /// body at all, such as a workflow dispatch.
    pub fn answer(
        &self,
        method: &str,
        endpoint: &str,
        body: &Value,
    ) -> Result<Vec<u8>, GitHubCommandError> {
        if self.github_com {
            let encoded = serde_json::to_vec(body)?;
            if let TransportAttempt::Handled(result) = http_transport::send(
                &self.origin,
                self.caller,
                method,
                endpoint,
                Some(&encoded),
                None,
            ) {
                return result;
            }
        }
        self.output(
            &["api", "--method", method, endpoint, "--input", "-"],
            Some(body),
            None,
        )
    }

    /// REST GET via HTTP when available, returning `Fallback` if the caller should run `gh`.
    pub(crate) fn try_http_get<T: DeserializeOwned>(&self, endpoint: &str) -> TransportAttempt<T> {
        if !self.github_com {
            return TransportAttempt::Fallback;
        }
        match http_transport::get(&self.origin, self.caller, endpoint, None) {
            TransportAttempt::Handled(Ok(bytes)) => {
                TransportAttempt::Handled(serde_json::from_slice(&bytes).map_err(Into::into))
            }
            TransportAttempt::Handled(Err(error)) => TransportAttempt::Handled(Err(error)),
            TransportAttempt::Fallback => TransportAttempt::Fallback,
        }
    }

    fn run<T: DeserializeOwned>(
        &self,
        arguments: &[&str],
        body: Option<&Value>,
        graphql_path: Option<&str>,
    ) -> Result<T, GitHubCommandError> {
        let answer = self.output(arguments, body, graphql_path)?;
        Ok(serde_json::from_slice(&answer)?)
    }

    fn output(
        &self,
        arguments: &[&str],
        body: Option<&Value>,
        graphql_path: Option<&str>,
    ) -> Result<Vec<u8>, GitHubCommandError> {
        let input = body.map(serde_json::to_vec).transpose()?;
        let mut child = self.spawn(arguments, input.is_some(), graphql_path)?;
        if let Some(input) = input {
            let written = child
                .stdin
                .take()
                .ok_or_else(|| GitHubCommandError::Missing("gh stdin".into()))?
                .write_all(&input);
            if let Err(error) = written {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        }
        let output = finish(child, DEADLINE)?;
        if !output.status.success() {
            return Err(GitHubCommandError::Command {
                code: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(output.stdout)
    }

    fn spawn(
        &self,
        arguments: &[&str],
        has_input: bool,
        graphql_path: Option<&str>,
    ) -> Result<Child, GitHubCommandError> {
        crate::request_counter::record_for_project(
            &self.origin,
            self.caller,
            crate::request_counter::operation(arguments, graphql_path),
        );
        let mut command = crate::command::gh_command();
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

/// Recognizes plain `gh api <endpoint>` and `gh api -H "Accept: <mime>" <endpoint>` calls
/// that can go directly through the pooled HTTPS transport with ETag caching.
fn parse_simple_api_get<'a>(arguments: &[&'a str]) -> Option<(&'a str, Option<&'a str>)> {
    match arguments {
        ["api", endpoint] if !endpoint.starts_with('-') => Some((endpoint, None)),
        ["api", "-H", header, endpoint] if !endpoint.starts_with('-') => {
            let accept = header
                .strip_prefix("Accept:")
                .or_else(|| header.strip_prefix("accept:"))?
                .trim();
            Some((endpoint, Some(accept)))
        }
        _ => None,
    }
}

/// Recognizes `gh api --method <METHOD> <endpoint>` calls with no body (such as DELETE).
fn parse_simple_api_method<'a>(arguments: &[&'a str]) -> Option<(&'a str, &'a str)> {
    match arguments {
        ["api", "--method", method, endpoint] if !endpoint.starts_with('-') => {
            Some((method, endpoint))
        }
        _ => None,
    }
}

/// Waits for `child` and collects what it printed, stopping it when it runs past `limit`.
fn finish(mut child: Child, limit: Duration) -> Result<Output, GitHubCommandError> {
    // Read on their own threads: a child that fills a pipe nobody reads never exits.
    let stdout = collect(child.stdout.take());
    let stderr = collect(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > limit {
            // The readers end when the pipes close; nothing waits for them.
            let _ = child.kill();
            let _ = child.wait();
            return Err(GitHubCommandError::TimedOut(limit.as_secs()));
        }
        std::thread::sleep(POLL);
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Reads `pipe` to its end on another thread.
fn collect<Pipe: Read + Send + 'static>(pipe: Option<Pipe>) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

fn origin_url(directory: &Path) -> Result<(String, bool), GitHubCommandError> {
    let repository = gix::discover(directory).map_err(|error| {
        GitHubCommandError::Invalid(format!("Could not open Git checkout: {error}"))
    })?;
    let remote = repository
        .find_remote("origin")
        .map_err(|error| GitHubCommandError::Invalid(format!("Could not read origin: {error}")))?;
    let mut url = remote
        .url(gix::remote::Direction::Fetch)
        .cloned()
        .ok_or_else(|| GitHubCommandError::Invalid("Origin has no fetch URL".into()))?;
    let github_com = url
        .host()
        .is_some_and(|host| host.eq_ignore_ascii_case("github.com"));
    // gh uses its own credential store. Git remote userinfo must never become
    // process arguments, even when the checkout's origin embeds credentials.
    url.set_user(None);
    url.set_password(None);
    // gh accepts SSH URIs, but not a stripped SCP-style host:path argument.
    if url.scheme == gix::url::Scheme::Ssh && !url.path.starts_with(b"/") {
        url.path = format!("/{}", url.path).into();
    }
    Ok((
        url.with_request_alternate_form(false)
            .to_bstring()
            .to_string(),
        github_com,
    ))
}

#[cfg(test)]
mod tests {
    use super::GitHubCommandError;

    #[test]
    fn classifies_local_tool_failures_apart_from_github_and_validation_errors() {
        // Risk (#581): project health must surface local AIRC issue tool failures such as
        // gh shim refusals or missing binaries without alerting on GitHub API errors or
        // agent input validation rejections.
        assert!(
            GitHubCommandError::Command {
                code: Some(1),
                message: "gh: Agents do not call gh directly. Use AIRC issue tools instead.".into(),
            }
            .is_local_tool_failure()
        );
        assert!(
            GitHubCommandError::Process(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "gh not found"
            ))
            .is_local_tool_failure()
        );
        assert!(GitHubCommandError::Missing("gh stdin".into()).is_local_tool_failure());
        assert!(
            GitHubCommandError::Invalid("GitHub HTTP worker stopped".into())
                .is_local_tool_failure()
        );

        assert!(
            !GitHubCommandError::Command {
                code: Some(404),
                message: "HTTP 404: Not Found".into(),
            }
            .is_local_tool_failure()
        );
        assert!(
            !GitHubCommandError::Command {
                code: Some(1),
                message: "gh: Not Found (HTTP 404)".into(),
            }
            .is_local_tool_failure()
        );
        assert!(
            !GitHubCommandError::Command {
                code: Some(1),
                message: "GraphQL: Could not resolve to an Issue with the number of 99999.".into(),
            }
            .is_local_tool_failure()
        );
        assert!(!GitHubCommandError::TimedOut(120).is_local_tool_failure());
        assert!(
            !GitHubCommandError::Missing("/data/repository/issue".into()).is_local_tool_failure()
        );
        assert!(
            !GitHubCommandError::Invalid("GitHub GraphQL errors: []".into())
                .is_local_tool_failure()
        );
        assert!(
            !GitHubCommandError::Invalid("Unknown label \"nope\".".into()).is_local_tool_failure()
        );
    }
}
