//! Post-land `[deploy]` step parsing, metadata reads, secure execution, and
//! `deploy_result` submission (Rosen R12, #594).
//!
//! Secret values are injected only into the spawned deploy child process after
//! stripping denied inherited environment variables, captured solely in memory
//! (never written to disk logs), and redacted before returning.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rosen_client::protocol::{
    DeployRecord, DeployStatus, ErrorCode, Event, EventKind, SubmitDeployResult,
};
use rosen_client::run_queue::{self, RunClass, RunQueue};
use rosen_core::{AgentId, Platform};
use rusqlite::{Connection, OpenFlags};
use velocity_core::agent_env::is_denied_name;
use velocity_core::deploy::{DeployRunState, DeployStepSummary, redact_secret_pairs};

use crate::checker::{self, Priority};
use crate::{ProjectServer, RosenError};

pub use rosen_client::protocol::DeployStatus as RosenDeployStatus;

const DEPLOY_COMMAND_LIMIT: Duration = Duration::from_secs(30 * 60);
const DEPLOY_SILENCE_LIMIT: Duration = Duration::from_secs(10 * 60);
const POLL: Duration = Duration::from_millis(250);
const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const MAX_REPORTED_BYTES: usize = 64 * 1024;

/// A `[deploy.<step>]` run due to execute on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeployDue {
    pub id: i64,
    pub step: String,
    pub commit: String,
    pub from: Option<String>,
    pub command: String,
    pub paths: Vec<String>,
    pub secrets: Vec<String>,
    pub platforms: Vec<String>,
}

/// A recorded `deploy_result` event from Rosen's event stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeployResultEvent {
    pub id: i64,
    pub step: String,
    pub commit: String,
    pub status: DeployStatus,
    pub output: String,
    pub submitter: Option<String>,
    pub at: Option<i64>,
}

/// Latest persisted run row for one deploy step in Rosen's `deploy_runs` table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeployRunRecord {
    pub id: i64,
    pub step: String,
    pub commit: String,
    pub command: String,
    pub state: DeployRunState,
    pub superseded_by: Option<String>,
    pub output: String,
    pub submitted_at: Option<i64>,
}

/// Outcome of executing a deploy command with output already secret-redacted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeployCommandOutcome {
    Passed {
        output: String,
    },
    Failed {
        exit_code: Option<i32>,
        stopped_reason: Option<String>,
        output: String,
    },
}

/// Parses a `deploy_due` event payload emitted by Rosen R12.
pub fn parse_deploy_due(event: &Event) -> Option<DeployDue> {
    if event.kind != EventKind::DeployDue {
        return None;
    }
    let detail = &event.detail;
    Some(DeployDue {
        id: detail.get("id")?.as_i64()?,
        step: detail.get("step")?.as_str()?.to_owned(),
        commit: detail.get("commit")?.as_str()?.to_owned(),
        from: detail
            .get("from")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        command: detail.get("command")?.as_str()?.to_owned(),
        paths: json_string_array(detail.get("paths")),
        secrets: json_string_array(detail.get("secrets")),
        platforms: json_string_array(detail.get("platforms")),
    })
}

/// Parses a `deploy_result` event payload emitted by Rosen R12.
pub fn parse_deploy_result(event: &Event) -> Option<DeployResultEvent> {
    if event.kind != EventKind::DeployResult {
        return None;
    }
    let detail = &event.detail;
    let status = match detail.get("status")?.as_str()? {
        "passed" => DeployStatus::Passed,
        "failed" => DeployStatus::Failed,
        "skipped" => DeployStatus::Skipped,
        _ => return None,
    };
    Some(DeployResultEvent {
        id: detail.get("id")?.as_i64()?,
        step: detail.get("step")?.as_str()?.to_owned(),
        commit: detail.get("commit")?.as_str()?.to_owned(),
        status,
        output: detail
            .get("output")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_owned(),
        submitter: detail
            .get("submitter")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
        at: detail.get("at").and_then(|value| value.as_i64()),
    })
}

/// Returns true when `platforms` is empty or at least one entry matches this host.
pub fn platform_matches_host(platforms: &[String]) -> bool {
    if platforms.is_empty() {
        return true;
    }
    let host = Platform::host();
    platforms
        .iter()
        .any(|raw| Platform::parse(raw).is_ok_and(|target| host.matches(&target)))
}

/// Reads the current landed `[deploy]` steps from the project's Rosen server.
pub async fn deploy_steps(server: &ProjectServer) -> Result<Vec<DeployStepSummary>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let config = client.project_config(None).await?.config;
    Ok(config
        .deploy
        .into_iter()
        .map(|step| DeployStepSummary {
            name: step.name,
            command: step.command,
            paths: step
                .paths
                .into_iter()
                .map(|pattern| pattern.as_str().to_owned())
                .collect(),
            secrets: step.secrets,
            platforms: step.platforms,
        })
        .collect())
}

/// Posts a `deploy_result` to Rosen. Returns `Ok(None)` when Rosen refused the
/// submission because the run was already superseded or finished differently.
pub async fn submit_deploy_result(
    server: &ProjectServer,
    id: i64,
    step: &str,
    commit: &str,
    status: DeployStatus,
    redacted_output: &str,
) -> Result<Option<DeployRecord>, RosenError> {
    let commit_id = crate::policy::object(commit)?;
    let client = crate::connection::requests(server.endpoint()).await?;
    let trimmed_output = tail_utf8(redacted_output, MAX_REPORTED_BYTES);
    let request = SubmitDeployResult {
        id,
        step: step.to_owned(),
        commit: commit_id,
        status,
        output: trimmed_output,
        submitter: AgentId::new(checker::CHECKER).ok(),
    };
    match client.deploy_result(request).await {
        Ok(record) => Ok(Some(record)),
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::Refused => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Reads currently `running` deploy rows from Rosen's `metadata.sqlite` (read-only)
/// so Velocity resumes in-flight deploys after a restart (#594).
pub fn running_deploys(server: &ProjectServer) -> Result<Vec<DeployDue>, RosenError> {
    let Some(connection) = open_deploy_db(server)? else {
        return Ok(Vec::new());
    };
    let mut statement = connection.prepare(
        "SELECT id, step, commit_id, from_commit, command, paths, secrets, platforms
         FROM deploy_runs
         WHERE state = 'running'
         ORDER BY green_seq ASC, step_index ASC, id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    let mut due = Vec::new();
    for row in rows {
        let (id, step, commit, from, command, paths, secrets, platforms) = row?;
        due.push(DeployDue {
            id,
            step,
            commit,
            from,
            command,
            paths: parse_json_strings(&paths),
            secrets: parse_json_strings(&secrets),
            platforms: parse_json_strings(&platforms),
        });
    }
    Ok(due)
}

/// Reads the newest deploy run row per step from Rosen's `metadata.sqlite` (read-only)
/// for Project Health reporting (#594).
pub fn latest_deploys_per_step(server: &ProjectServer) -> Result<Vec<DeployRunRecord>, RosenError> {
    let Some(connection) = open_deploy_db(server)? else {
        return Ok(Vec::new());
    };
    let mut statement = connection.prepare(
        "SELECT id, step, commit_id, command, state, superseded_by, output, submitted_at
         FROM deploy_runs
         WHERE id IN (SELECT MAX(id) FROM deploy_runs GROUP BY step)
         ORDER BY step_index ASC, id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, Option<i64>>(7)?,
        ))
    })?;
    let mut records = Vec::new();
    for row in rows {
        let (id, step, commit, command, state_str, superseded_by, output, submitted_at) = row?;
        let Some(state) = parse_run_state(&state_str) else {
            continue;
        };
        records.push(DeployRunRecord {
            id,
            step,
            commit,
            command,
            state,
            superseded_by,
            output,
            submitted_at,
        });
    }
    Ok(records)
}

/// Runs one deploy command in `folder` under a background run-queue turn, passing
/// ONLY `secrets` in addition to non-credential inherited environment variables.
/// Captures output in memory and redacts all secret values before returning.
pub fn run_deploy_command(
    folder: &Path,
    command: &str,
    secrets: &[(String, String)],
) -> Result<DeployCommandOutcome, RosenError> {
    let note = format!("deploy in {}", folder.display());
    let turn = RunQueue::machine()
        .and_then(|queue| queue.turn(RunClass::Verify, &note))
        .ok();
    while crate::memory::is_low() {
        std::thread::sleep(POLL);
    }
    let mut shell = build_deploy_shell(folder, command, turn.is_some(), secrets);
    let mut child = shell.spawn().map_err(|source| RosenError::Io {
        action: "run deploy command in",
        path: folder.to_path_buf(),
        source,
    })?;
    let capture = Arc::new(Mutex::new(CaptureBuffer::new()));
    let stdout_reader = child
        .stdout
        .take()
        .map(|pipe| spawn_pipe_reader(pipe, Arc::clone(&capture)));
    let stderr_reader = child
        .stderr
        .take()
        .map(|pipe| spawn_pipe_reader(pipe, Arc::clone(&capture)));
    let outcome = wait_for_deploy(&mut child, &capture, folder)?;
    if let Some(handle) = stdout_reader {
        let _ = handle.join();
    }
    if let Some(handle) = stderr_reader {
        let _ = handle.join();
    }
    let raw_output = capture
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .to_output();
    let mut output = redact_secret_pairs(&raw_output, secrets);
    Ok(match outcome {
        WaitResult::Exited(0) => DeployCommandOutcome::Passed { output },
        WaitResult::Exited(code) => DeployCommandOutcome::Failed {
            exit_code: Some(code),
            stopped_reason: None,
            output,
        },
        WaitResult::Stopped { reason, process } => {
            let mut note = format!("\n[Velocity] Deploy command {reason}.");
            if let Some(process) = &process {
                note.push_str(&format!(" Process: {process}."));
            }
            note.push('\n');
            output.push_str(&note);
            DeployCommandOutcome::Failed {
                exit_code: None,
                stopped_reason: Some(reason.to_owned()),
                output,
            }
        }
    })
}

fn build_deploy_shell(
    folder: &Path,
    command: &str,
    held_turn: bool,
    secrets: &[(String, String)],
) -> std::process::Command {
    let mut shell = checker::shell(command, Priority::Background);
    for (key, _) in std::env::vars_os() {
        if let Some(name) = key.to_str()
            && is_denied_name(name)
        {
            shell.env_remove(&key);
        }
    }
    if held_turn {
        shell.env(run_queue::HELD_ENV, RunClass::Verify.name());
    }
    for (name, value) in secrets {
        shell.env(name, value);
    }
    shell
        .current_dir(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    shell
}

enum WaitResult {
    Exited(i32),
    Stopped {
        reason: &'static str,
        process: Option<String>,
    },
}

fn wait_for_deploy(
    child: &mut Child,
    capture: &Arc<Mutex<CaptureBuffer>>,
    folder: &Path,
) -> Result<WaitResult, RosenError> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|source| RosenError::Io {
            action: "wait for deploy command in",
            path: folder.to_path_buf(),
            source,
        })? {
            return Ok(WaitResult::Exited(status.code().unwrap_or(1)));
        }
        if started.elapsed() >= DEPLOY_COMMAND_LIMIT {
            let process = checker::active_descendant_process(child.id());
            checker::stop_tree(child);
            return Ok(WaitResult::Stopped {
                reason: "stopped after 30 minutes time limit",
                process,
            });
        }
        let silent = capture
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .last_output
            .elapsed();
        if silent >= DEPLOY_SILENCE_LIMIT {
            let process = checker::active_descendant_process(child.id());
            checker::stop_tree(child);
            return Ok(WaitResult::Stopped {
                reason: "stopped after 10 minutes without new output",
                process,
            });
        }
        std::thread::sleep(POLL);
    }
}

struct CaptureBuffer {
    bytes: Vec<u8>,
    truncated: bool,
    last_output: Instant,
}

impl CaptureBuffer {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            truncated: false,
            last_output: Instant::now(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        self.last_output = Instant::now();
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > MAX_CAPTURE_BYTES {
            let excess = self.bytes.len() - MAX_CAPTURE_BYTES;
            self.bytes.drain(..excess);
            self.truncated = true;
        }
    }

    fn to_output(&self) -> String {
        let text = String::from_utf8_lossy(&self.bytes);
        if self.truncated {
            format!("[output truncated to last {MAX_CAPTURE_BYTES} bytes]\n{text}")
        } else {
            text.into_owned()
        }
    }
}

fn spawn_pipe_reader<R: Read + Send + 'static>(
    mut pipe: R,
    capture: Arc<Mutex<CaptureBuffer>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    capture
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(&buffer[..read]);
                }
            }
        }
    })
}

fn open_deploy_db(server: &ProjectServer) -> Result<Option<Connection>, RosenError> {
    let path = server.data_dir().join("metadata.sqlite");
    if !path.is_file() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'deploy_runs')",
        [],
        |row| row.get(0),
    )?;
    Ok(exists.then_some(connection))
}

fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_json_strings(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn parse_run_state(raw: &str) -> Option<DeployRunState> {
    match raw {
        "pending" => Some(DeployRunState::Pending),
        "running" => Some(DeployRunState::Running),
        "passed" => Some(DeployRunState::Passed),
        "failed" => Some(DeployRunState::Failed),
        "skipped" => Some(DeployRunState::Skipped),
        _ => None,
    }
}

fn tail_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let start = text.ceil_char_boundary(text.len().saturating_sub(max_bytes));
    text[start..].to_owned()
}
