//! Bounded queue-turn acquisition for agent `queue_run` commands (#516).
//!
//! Waiting for a Rosen run-queue turn (or a light Rust slot/memory) must not hang
//! silently forever or retry behind the agent's back: on timeout it reports the exact
//! wait reason, queue line and memory readings back to the owning thread.

use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rosen_client::run_queue::{Entry, POLL, RunClass, RunQueue, Turn};
use velocity_core::queued_runs::QueuedRunClass;

use crate::agent_run::{
    AgentRun, LIGHT_MEMORY, LIGHT_SLOTS, light_target, try_light_slot, try_lock_file,
};

pub const HELD_ENV: &str = rosen_client::run_queue::HELD_ENV;
pub const QUEUE_ENV: &str = rosen_client::run_queue::QUEUE_ENV;
pub const THREAD_VARIABLES: [&str; 3] = rosen_client::run_queue::THREAD_VARIABLES;

/// How long a queued command waits for a queue turn before reporting the block.
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

const GIB: f64 = (1_u64 << 30) as f64;

/// A turn held until the queued command finishes or is cancelled.
pub enum QueuedTurn {
    /// A full turn in Rosen's machine run queue.
    Rosen(Turn),
    /// A light slot for `cargo fmt` or `cargo metadata`.
    Utility { _slot: File },
    /// A project target lock and light slot with 3 GiB memory headroom verified.
    Light {
        _project: File,
        _slot: File,
        target: PathBuf,
    },
}

/// Parameters for acquiring a bounded turn in the run queue.
pub struct QueuedTurnRequest<'a> {
    pub policy: AgentRun,
    pub class: QueuedRunClass,
    pub worktree: &'a Path,
    pub directory: &'a Path,
    pub command_line: &'a str,
    pub queue_override: Option<&'a Path>,
    pub timeout: Duration,
}

/// Snapshot of the machine queue line and memory when a queued run times out waiting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueTimeoutReport {
    pub waited: Duration,
    pub class: String,
    pub waiting_for: Option<String>,
    pub running: Vec<String>,
    pub waiting: Vec<String>,
    pub memory_available: Option<u64>,
    pub memory_reserved: Option<u64>,
    pub memory_reserve: Option<u64>,
}

impl QueueTimeoutReport {
    fn read(queue: &RunQueue, class: &str, waited: Duration, waiting_for: Option<String>) -> Self {
        let entries = queue.line().unwrap_or_default();
        let (running, waiting) = describe_line(&entries);
        let headroom = queue.memory_headroom();
        let memory_reserved = queue.running_budget().ok().map(|budget| budget.memory);
        Self {
            waited,
            class: class.to_owned(),
            waiting_for,
            running,
            waiting,
            memory_available: headroom.available,
            memory_reserved,
            memory_reserve: headroom.reserve,
        }
    }
}

impl fmt::Display for QueueTimeoutReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let waited = format_duration(self.waited);
        writeln!(
            formatter,
            "Timed out after {waited} waiting for a `{}` run-queue turn (not retried).",
            self.class
        )?;
        if let Some(reason) = &self.waiting_for {
            writeln!(formatter, "Wait reason: {reason}")?;
        }
        let running = if self.running.is_empty() {
            "none".to_owned()
        } else {
            self.running.join("; ")
        };
        let waiting = if self.waiting.is_empty() {
            "none".to_owned()
        } else {
            self.waiting.join("; ")
        };
        writeln!(formatter, "Queue line · running: {running}")?;
        writeln!(formatter, "Queue line · waiting: {waiting}")?;
        write!(
            formatter,
            "Memory: {} available; {} reserved by runs; {} reserve",
            format_bytes(self.memory_available),
            format_bytes(self.memory_reserved),
            format_bytes(self.memory_reserve),
        )
    }
}

#[derive(Debug)]
pub enum QueuedTurnError {
    Cancelled,
    TimedOut(Box<QueueTimeoutReport>),
    Io(io::Error),
}

impl From<io::Error> for QueuedTurnError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Acquires a turn for one queued command, cancelling promptly or timing out with a
/// diagnostic report of the queue line and memory.
pub fn acquire(
    request: &QueuedTurnRequest<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<QueuedTurn, QueuedTurnError> {
    let queue = match request.queue_override {
        Some(path) => RunQueue::at(path),
        None => RunQueue::machine()?,
    };
    match request.policy {
        AgentRun::Ordinary => {
            let note = format!(
                "{} in {}",
                request.command_line,
                request.directory.display()
            );
            acquire_rosen(
                &queue,
                run_class(request.class),
                &note,
                request.timeout,
                &cancelled,
            )
        }
        AgentRun::RustUtility => acquire_utility(&queue, request.timeout, &cancelled),
        AgentRun::RustCheck | AgentRun::Bindings => {
            acquire_light(&queue, request.worktree, request.timeout, &cancelled)
        }
    }
}

fn acquire_rosen(
    queue: &RunQueue,
    class: RunClass,
    note: &str,
    timeout: Duration,
    cancelled: &impl Fn() -> bool,
) -> Result<QueuedTurn, QueuedTurnError> {
    let waiter = queue.wait(class, note)?;
    let started = Instant::now();
    loop {
        if cancelled() {
            return Err(QueuedTurnError::Cancelled);
        }
        if let Some(turn) = waiter.try_start()? {
            return Ok(QueuedTurn::Rosen(turn));
        }
        if started.elapsed() >= timeout {
            let waited = started.elapsed();
            let waiting_for = waiter.waiting_for();
            drop(waiter);
            return Err(QueuedTurnError::TimedOut(Box::new(
                QueueTimeoutReport::read(queue, class.name(), waited, waiting_for),
            )));
        }
        std::thread::sleep(POLL);
    }
}

fn acquire_utility(
    queue: &RunQueue,
    timeout: Duration,
    cancelled: &impl Fn() -> bool,
) -> Result<QueuedTurn, QueuedTurnError> {
    let started = Instant::now();
    loop {
        if cancelled() {
            return Err(QueuedTurnError::Cancelled);
        }
        if let Some(slot) = try_light_slot()? {
            return Ok(QueuedTurn::Utility { _slot: slot });
        }
        if started.elapsed() >= timeout {
            return Err(QueuedTurnError::TimedOut(Box::new(
                QueueTimeoutReport::read(
                    queue,
                    "light-rust",
                    started.elapsed(),
                    Some(format!(
                        "waiting for a free light Rust slot ({LIGHT_SLOTS} slots)"
                    )),
                ),
            )));
        }
        std::thread::sleep(POLL);
    }
}

fn acquire_light(
    queue: &RunQueue,
    worktree: &Path,
    timeout: Duration,
    cancelled: &impl Fn() -> bool,
) -> Result<QueuedTurn, QueuedTurnError> {
    let target = light_target(worktree);
    std::fs::create_dir_all(&target)?;
    let started = Instant::now();
    let project_lock = target.join(".velocity-light-run.lock");
    let project = wait_step(
        queue,
        started,
        timeout,
        cancelled,
        "waiting for the project's shared Rust target lock",
        || try_lock_file(&project_lock),
    )?;
    let slot = wait_step(
        queue,
        started,
        timeout,
        cancelled,
        &format!("waiting for a free light Rust slot ({LIGHT_SLOTS} slots)"),
        try_light_slot,
    )?;
    wait_step(
        queue,
        started,
        timeout,
        cancelled,
        "waiting for 3.0 GiB memory headroom above Rosen's reserve",
        || Ok(queue.memory_headroom().fits(LIGHT_MEMORY).then_some(())),
    )?;
    Ok(QueuedTurn::Light {
        _project: project,
        _slot: slot,
        target,
    })
}

fn wait_step<T>(
    queue: &RunQueue,
    started: Instant,
    timeout: Duration,
    cancelled: &impl Fn() -> bool,
    reason: &str,
    mut attempt: impl FnMut() -> io::Result<Option<T>>,
) -> Result<T, QueuedTurnError> {
    loop {
        if cancelled() {
            return Err(QueuedTurnError::Cancelled);
        }
        if let Some(value) = attempt()? {
            return Ok(value);
        }
        if started.elapsed() >= timeout {
            return Err(QueuedTurnError::TimedOut(Box::new(
                QueueTimeoutReport::read(
                    queue,
                    "light-rust",
                    started.elapsed(),
                    Some(reason.to_owned()),
                ),
            )));
        }
        std::thread::sleep(POLL);
    }
}

fn run_class(class: QueuedRunClass) -> RunClass {
    match class {
        QueuedRunClass::Build => RunClass::Build,
        QueuedRunClass::Landing => RunClass::Landing,
        QueuedRunClass::Verify => RunClass::Verify,
    }
}

fn describe_line(entries: &[Entry]) -> (Vec<String>, Vec<String>) {
    let mut running = Vec::new();
    let mut waiting = Vec::new();
    for entry in entries {
        if entry.running {
            let note = entry.note.trim();
            if note.is_empty() {
                running.push(entry.class.name().to_owned());
            } else {
                running.push(format!("{}: {note}", entry.class.name()));
            }
        } else {
            match &entry.waiting_for {
                Some(reason) => waiting.push(format!("{} ({reason})", entry.class.name())),
                None => waiting.push(entry.class.name().to_owned()),
            }
        }
    }
    (running, waiting)
}

fn format_bytes(bytes: Option<u64>) -> String {
    match bytes {
        Some(bytes) => format!("{:.1} GiB", bytes as f64 / GIB),
        None => "unavailable".to_owned(),
    }
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#516): a queued run blocked in a starved run queue must time out and report
    // the queue line and memory readings to the thread instead of waiting forever.
    #[test]
    fn timeout_reports_queue_line_and_memory() -> Result<(), Box<dyn std::error::Error>> {
        let queue_dir = tempfile::tempdir()?;
        let queue = RunQueue::at(queue_dir.path());
        queue.set_slots(RunClass::Build, Some(1))?;
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(queue_dir.path().join("run-build.lock"))?;
        holder.lock()?;
        std::fs::write(
            queue_dir.path().join("run-build.note"),
            "npm run build in workspace-a",
        )?;

        let request = QueuedTurnRequest {
            policy: AgentRun::Ordinary,
            class: QueuedRunClass::Build,
            worktree: queue_dir.path(),
            directory: queue_dir.path(),
            command_line: "npm run check",
            queue_override: Some(queue_dir.path()),
            timeout: Duration::from_millis(80),
        };
        let result = acquire(&request, || false);
        drop(holder);
        let Err(QueuedTurnError::TimedOut(report)) = result else {
            return Err("expected a queue wait timeout".into());
        };
        let rendered = report.to_string();
        assert!(rendered.contains("not retried"), "{rendered}");
        assert!(
            rendered.contains("build: npm run build in workspace-a"),
            "{rendered}"
        );
        assert!(rendered.contains("Memory:"), "{rendered}");
        assert!(rendered.contains("reserve"), "{rendered}");
        Ok(())
    }
}
