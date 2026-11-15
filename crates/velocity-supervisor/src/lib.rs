//! Recovery after a process-wide allocation abort (SPEC.md §3.2, #395).
//!
//! The idle parent never starts Tauri, ACP sessions or the webview. A Rust allocation
//! failure aborts the worker; its owned agent jobs close, and this parent retries the
//! app after a delay. This recovers work once the machine has memory again rather
//! than trying to catch an allocation failure inside an already-aborting process.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

const WORKER_PREFIX: &str = "--velocity-supervised-by=";
const FIRST_RETRY: Duration = Duration::from_secs(5);
const MAXIMUM_RETRY: Duration = Duration::from_secs(60);
const HEALTHY_RUN: Duration = Duration::from_secs(300);

/// Returns `true` after supervising the desktop app until its normal exit, or
/// `false` inside the supervised worker, which must continue into Tauri.
/// Call after handling MCP/warming modes: those belong to their ACP caller.
pub fn launch_current() -> io::Result<bool> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if let Some(parent) = arguments
        .first()
        .and_then(|argument| worker_parent(argument))
        && actual_parent()? == parent
    {
        return Ok(false);
    }
    // An updater copies the worker's arguments, but its new process belongs to
    // the installer/old worker, not the old supervisor. Give it a new supervisor.
    let mut worker_arguments = vec![OsString::from(format!(
        "{WORKER_PREFIX}{}",
        std::process::id()
    ))];
    worker_arguments.extend(arguments.into_iter().filter(|argument| {
        !argument
            .to_str()
            .is_some_and(|text| text.starts_with(WORKER_PREFIX))
    }));
    Supervisor::new(&std::env::current_exe()?, worker_arguments).run();
    Ok(true)
}

fn worker_parent(argument: &OsStr) -> Option<u32> {
    argument.to_str()?.strip_prefix(WORKER_PREFIX)?.parse().ok()
}

fn actual_parent() -> io::Result<u32> {
    let mut system = System::new().map_err(io::Error::other)?;
    let current = Pid::from_u32(std::process::id());
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[current]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system
        .process(current)
        .and_then(|process| process.parent())
        .map(|parent| parent.as_u32())
        // An unreadable process must fail this worker, not recursively supervise
        // it: the existing parent will retry after memory pressure subsides.
        .ok_or_else(|| io::Error::other("Could not identify Velocity's supervisor"))
}

/// One desktop worker at a time. The command and arguments are retained while
/// waiting, with no GUI state, growing history or per-tick polling allocations.
pub struct Supervisor {
    command: Command,
}

impl Supervisor {
    pub fn new(program: &Path, arguments: impl IntoIterator<Item = OsString>) -> Self {
        let mut command = hidden_command(program);
        command.args(arguments);
        Self { command }
    }

    /// A clean close (including the updater's exit) disarms recovery. Aborts,
    /// signals and failed launches retry indefinitely with 5–60 second backoff:
    /// memory exhaustion must not turn into a fast spawn/crash loop or give up.
    pub fn run(&mut self) {
        let mut retry = FIRST_RETRY;
        loop {
            let started = Instant::now();
            match self.command.spawn() {
                Ok(mut worker) => {
                    let status = wait_for_exit(&mut worker);
                    if status.success() {
                        return;
                    }
                    if started.elapsed() >= HEALTHY_RUN {
                        retry = FIRST_RETRY;
                    }
                    let _ = writeln!(
                        io::stderr(),
                        "Velocity recovery: app exited {status}; retrying in {retry:?}"
                    );
                }
                Err(error) => {
                    // Formatting a localized OS error can allocate. A raw code
                    // and a non-panicking write remain usable during pressure.
                    let _ = writeln!(
                        io::stderr(),
                        "Velocity recovery: app launch OS error {:?}; retrying in {retry:?}",
                        error.raw_os_error()
                    );
                }
            }
            std::thread::sleep(retry);
            retry = retry.saturating_mul(2).min(MAXIMUM_RETRY);
        }
    }
}

fn wait_for_exit(worker: &mut Child) -> ExitStatus {
    loop {
        match worker.wait() {
            Ok(status) => return status,
            // Never start a second app while the first one's exit is uncertain.
            Err(_) => std::thread::sleep(FIRST_RETRY),
        }
    }
}

#[allow(clippy::disallowed_methods)]
fn hidden_command(program: &Path) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}
