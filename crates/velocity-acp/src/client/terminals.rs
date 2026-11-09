//! The `terminal/*` client capability: commands the agent runs through Velocity, in the
//! worktree unless the agent names another folder (§3.2 Tool access: not a jail).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use agent_client_protocol::v1::schema::{
    CreateTerminalRequest, CreateTerminalResponse, TerminalExitStatus, TerminalOutputResponse,
};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use super::ClientError;
use super::files::resolve_in_worktree;
use super::process::hide_console_window;
#[cfg(windows)]
use super::program_path::find_program;
use crate::retained_output_start;

/// Output kept so far, truncated from the front at a character boundary
/// (`terminal_output::retained_output_start`).
struct OutputBuffer {
    bytes: Vec<u8>,
    limit: Option<usize>,
    truncated: bool,
}

impl OutputBuffer {
    fn append(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if let Some(limit) = self.limit {
            let start = retained_output_start(&self.bytes, limit);
            if start > 0 {
                self.bytes.drain(..start);
                self.truncated = true;
            }
        }
    }
}

struct Terminal {
    output: Arc<Mutex<OutputBuffer>>,
    exit: watch::Receiver<Option<TerminalExitStatus>>,
    /// Sending kills the command. Dropping it on release kills it too.
    kill: Mutex<Option<oneshot::Sender<()>>>,
}

/// The terminals one agent session has created.
pub struct Terminals {
    worktree: PathBuf,
    next_id: AtomicU64,
    terminals: Mutex<HashMap<String, Arc<Terminal>>>,
}

fn lock<Value>(mutex: &Mutex<Value>) -> MutexGuard<'_, Value> {
    // A panic while holding the lock leaves plain data behind, still safe to read.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Terminals {
    pub fn new(worktree: PathBuf) -> Self {
        Terminals {
            worktree,
            next_id: AtomicU64::new(1),
            terminals: Mutex::new(HashMap::new()),
        }
    }

    pub fn create(
        &self,
        request: CreateTerminalRequest,
    ) -> Result<CreateTerminalResponse, ClientError> {
        let folder = match &request.cwd {
            Some(folder) => resolve_in_worktree(&self.worktree, folder)?,
            None => self.worktree.clone(),
        };
        let mut command = shell_command(&request.command, &request.args);
        command
            .current_dir(&folder)
            .envs(
                request
                    .env
                    .iter()
                    .map(|variable| (&variable.name, &variable.value)),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_console_window(&mut command);
        let child = command.spawn().map_err(|source| ClientError::Spawn {
            program: request.command.clone(),
            source,
        })?;
        let limit = request
            .output_byte_limit
            .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX));
        let terminal = start_terminal(child, limit);
        let id = format!("terminal-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        lock(&self.terminals).insert(id.clone(), Arc::new(terminal));
        Ok(CreateTerminalResponse::new(id))
    }

    fn find(&self, id: &str) -> Result<Arc<Terminal>, ClientError> {
        lock(&self.terminals)
            .get(id)
            .cloned()
            .ok_or_else(|| ClientError::UnknownTerminal(id.to_owned()))
    }

    pub fn output(&self, id: &str) -> Result<TerminalOutputResponse, ClientError> {
        let terminal = self.find(id)?;
        let buffer = lock(&terminal.output);
        let text = String::from_utf8_lossy(&buffer.bytes).into_owned();
        let exit = terminal.exit.borrow().clone();
        Ok(TerminalOutputResponse::new(text, buffer.truncated).exit_status(exit))
    }

    pub async fn wait_for_exit(&self, id: &str) -> Result<TerminalExitStatus, ClientError> {
        let mut exit = self.find(id)?.exit.clone();
        let status = exit
            .wait_for(Option::is_some)
            .await
            .map_err(|_| ClientError::UnknownTerminal(id.to_owned()))?;
        Ok(status.clone().unwrap_or_default())
    }

    pub fn kill(&self, id: &str) -> Result<(), ClientError> {
        let terminal = self.find(id)?;
        if let Some(kill) = lock(&terminal.kill).take() {
            // The command may already have exited; there is nothing left to kill then.
            let _ = kill.send(());
        }
        Ok(())
    }

    /// Kills the command if it still runs and forgets the terminal.
    pub fn release(&self, id: &str) -> Result<(), ClientError> {
        lock(&self.terminals)
            .remove(id)
            .map(drop)
            .ok_or_else(|| ClientError::UnknownTerminal(id.to_owned()))
    }
}

/// A command with arguments runs as given. A command without arguments is a command line,
/// as an agent's shell tool writes it, and runs through the user's shell.
#[cfg(windows)]
fn shell_command(command: &str, args: &[String]) -> Command {
    if args.is_empty() {
        let shell = std::env::var_os("ComSpec").unwrap_or_else(|| "cmd.exe".into());
        let mut built = Command::new(shell);
        // `/S /C "…"`: cmd strips the outer quotes and runs the line exactly as written.
        built.raw_arg(format!("/D /S /C \"{command}\""));
        built
    } else {
        let mut built = Command::new(find_program(command.as_ref()));
        built.args(args);
        built
    }
}

#[cfg(not(windows))]
fn shell_command(command: &str, args: &[String]) -> Command {
    if args.is_empty() {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
        let mut built = Command::new(shell);
        built.arg("-c").arg(command);
        built
    } else {
        let mut built = Command::new(command);
        built.args(args);
        built
    }
}

fn start_terminal(mut child: Child, limit: Option<usize>) -> Terminal {
    let output = Arc::new(Mutex::new(OutputBuffer {
        bytes: Vec::new(),
        limit,
        truncated: false,
    }));
    let readers = [
        child
            .stdout
            .take()
            .map(|stdout| pump(stdout, Arc::clone(&output))),
        child
            .stderr
            .take()
            .map(|stderr| pump(stderr, Arc::clone(&output))),
    ];
    let (exit_sender, exit) = watch::channel(None);
    let (kill, kill_requested) = oneshot::channel();
    tokio::spawn(watch_child(child, kill_requested, readers, exit_sender));
    Terminal {
        output,
        exit,
        kill: Mutex::new(Some(kill)),
    }
}

fn pump(
    mut stream: impl AsyncRead + Unpin + Send + 'static,
    output: Arc<Mutex<OutputBuffer>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut chunk = [0_u8; 8192];
        while let Ok(read) = stream.read(&mut chunk).await {
            if read == 0 {
                break;
            }
            lock(&output).append(&chunk[..read]);
        }
    })
}

async fn watch_child(
    mut child: Child,
    kill_requested: oneshot::Receiver<()>,
    readers: [Option<JoinHandle<()>>; 2],
    exit: watch::Sender<Option<TerminalExitStatus>>,
) {
    // A dropped sender (the terminal was released) kills the command as well.
    let status = tokio::select! {
        status = child.wait() => status,
        _ = kill_requested => {
            let _ = child.start_kill();
            child.wait().await
        }
    };
    // Report the exit only after all output has been read, so `terminal/output` after
    // `wait_for_exit` sees all of it.
    for reader in readers.into_iter().flatten() {
        let _ = reader.await;
    }
    let status = status.map_or_else(|_| TerminalExitStatus::new(), |status| exit_status(&status));
    let _ = exit.send(Some(status));
}

fn exit_status(status: &ExitStatus) -> TerminalExitStatus {
    // Windows exit codes are 32-bit unsigned (NTSTATUS values such as 0xC0000005 included);
    // Rust reports them as `i32`, so keep the bits.
    let code = status
        .code()
        .map(|code| u32::from_ne_bytes(code.to_ne_bytes()));
    TerminalExitStatus::new()
        .exit_code(code)
        .signal(signal_name(status))
}

#[cfg(unix)]
fn signal_name(status: &ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    status.signal().map(|signal| signal.to_string())
}

#[cfg(not(unix))]
fn signal_name(_status: &ExitStatus) -> Option<String> {
    None
}
