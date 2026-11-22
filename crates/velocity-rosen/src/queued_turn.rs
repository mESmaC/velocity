//! Bounded queue-turn acquisition, queue estimation, and Rosen workspace identity for agent
//! `queue_run` commands (#516, #536).
//!
//! Waiting for a Rosen run-queue turn (or a light Rust slot/memory) must not hang
//! silently forever or retry behind the agent's back: on timeout it reports the exact
//! wait reason, queue line and memory readings back to the owning thread.

use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rosen_client::run_queue::{Entry, POLL, RunClass, RunQueue, Turn, Waiter};
use serde::{Deserialize, Serialize};
use velocity_core::queued_runs::QueuedRunClass;

use crate::agent_run::{
    AgentRun, LIGHT_MEMORY, LIGHT_SLOTS, busy_light_slots, busy_light_targets, light_targets,
    try_light_slot, try_lock_light_target,
};

pub const HELD_ENV: &str = rosen_client::run_queue::HELD_ENV;
pub const QUEUE_ENV: &str = rosen_client::run_queue::QUEUE_ENV;
pub const THREAD_VARIABLES: [&str; 3] = rosen_client::run_queue::THREAD_VARIABLES;

/// How long a queued command waits for a queue turn before reporting the block.
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

const GIB: f64 = (1_u64 << 30) as f64;
const UTILITY_RUN_ESTIMATE_SECS: u64 = 10;
const LIGHT_RUN_ESTIMATE_SECS: u64 = 45;
const ORDINARY_BUILD_ESTIMATE_SECS: u64 = 60;
const ORDINARY_VERIFY_ESTIMATE_SECS: u64 = 180;

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

/// Live handle to an ordinary run's waiting ticket in Rosen's machine run queue (#665).
///
/// Lets `run_status` inspect the ticket's actual place ahead in line via `Waiter::ahead`
/// without extending the underlying `Waiter` file lock past turn acquisition or exit.
#[derive(Clone)]
pub struct QueueTicket {
    queue: RunQueue,
    class: QueuedRunClass,
    active: Arc<AtomicBool>,
    waiter: Arc<Mutex<Option<Waiter>>>,
}

impl QueueTicket {
    fn new(queue: RunQueue, class: QueuedRunClass, waiter: Waiter) -> Self {
        Self {
            queue,
            class,
            active: Arc::new(AtomicBool::new(true)),
            waiter: Arc::new(Mutex::new(Some(waiter))),
        }
    }

    /// Whether this run still holds its waiting ticket in Rosen's machine run queue.
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Estimates queue position and wait from this ticket's own place in line, excluding
    /// the ticket itself and any later waiters. Returns `None` once the ticket has left
    /// the waiting line.
    pub fn estimate(&self) -> Option<QueueEstimate> {
        if !self.is_active() {
            return None;
        }
        let guard = self.waiter.lock().unwrap_or_else(PoisonError::into_inner);
        let waiter = guard.as_ref()?;
        let entries = waiter.ahead().unwrap_or_default();
        let waiter_reason = waiter.waiting_for();
        Some(estimate_ordinary_entries(
            &self.queue,
            self.class,
            &entries,
            0,
            waiter_reason,
        ))
    }

    fn clear(&self) {
        self.active.store(false, Ordering::Release);
        let _ = self
            .waiter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

struct ClearTicketOnDrop(QueueTicket);

impl Drop for ClearTicketOnDrop {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// Live queue position and estimated wait for a queued command (#536).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEstimate {
    /// 1-based position in line (1 = next to start once eligible).
    pub position: u32,
    /// Estimated wait in seconds after the run enters the queue (0 when free now).
    pub estimated_wait_seconds: u64,
    /// Human-readable estimated wait (e.g. `immediate`, `~45s`, `~2m 0s`).
    pub estimated_wait: String,
    /// Why the run would wait right now, if it cannot start immediately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_reason: Option<String>,
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

/// Returns `(change_id, head_tree)` for a Rosen workspace, when `.rosen/workspace.json` exists.
pub fn workspace_head(worktree: &Path) -> Option<(String, String)> {
    let marker = rosen_client::snapshot::WorkspaceMarker::read(worktree).ok()?;
    Some((marker.change.to_string(), marker.tree.to_string()))
}

/// Concise machine-wide queue depth for the opening session card (#534).
pub fn queue_depth_summary() -> String {
    let busy_light = busy_light_slots();
    let line = RunQueue::machine()
        .ok()
        .and_then(|queue| queue.line().ok())
        .unwrap_or_default();
    let running = line.iter().filter(|entry| entry.running).count();
    let waiting = line.iter().filter(|entry| !entry.running).count();
    format!("{busy_light}/{LIGHT_SLOTS} light slots busy, {running} running, {waiting} waiting")
}

/// Estimates queue position, wait duration, and wait reason for a run under `policy` and `class`.
pub fn estimate(
    policy: AgentRun,
    class: QueuedRunClass,
    worktree: &Path,
    queue_override: Option<&Path>,
    session_pending_ahead: usize,
) -> QueueEstimate {
    let queue = match queue_override {
        Some(path) => Ok(RunQueue::at(path)),
        None => RunQueue::machine(),
    };
    match policy {
        AgentRun::Ordinary => estimate_ordinary(queue.ok().as_ref(), class, session_pending_ahead),
        AgentRun::RustUtility => estimate_utility(session_pending_ahead),
        AgentRun::RustCheck | AgentRun::Bindings | AgentRun::RegisteredGenerator => {
            estimate_light(queue.ok().as_ref(), worktree, session_pending_ahead)
        }
    }
}

fn ordinary_per_run_seconds(class: QueuedRunClass) -> u64 {
    match class {
        QueuedRunClass::Verify => ORDINARY_VERIFY_ESTIMATE_SECS,
        QueuedRunClass::Build | QueuedRunClass::Landing => ORDINARY_BUILD_ESTIMATE_SECS,
    }
}

fn ordinary_queue_reason(
    target_class: RunClass,
    running_higher: usize,
    running_same: usize,
    slots: usize,
    waiting_ahead: usize,
    memory_blocked: bool,
) -> Option<String> {
    if running_higher > 0 {
        Some("yielding to more important runs".to_owned())
    } else if running_same >= slots {
        Some(format!("waiting for a free {} slot", target_class.name()))
    } else if waiting_ahead > 0 {
        Some(format!(
            "{waiting_ahead} queued {} ahead in `{}` queue",
            if waiting_ahead == 1 { "run" } else { "runs" },
            target_class.name()
        ))
    } else if memory_blocked {
        Some("waiting for memory headroom above Rosen's reserve".to_owned())
    } else {
        None
    }
}

fn estimate_ordinary(
    queue: Option<&RunQueue>,
    class: QueuedRunClass,
    session_pending_ahead: usize,
) -> QueueEstimate {
    let Some(queue) = queue else {
        return build_estimate(
            session_pending_ahead,
            0,
            1,
            ordinary_per_run_seconds(class),
            None,
        );
    };
    let entries = queue.line().unwrap_or_default();
    estimate_ordinary_entries(queue, class, &entries, session_pending_ahead, None)
}

fn estimate_ordinary_entries(
    queue: &RunQueue,
    class: QueuedRunClass,
    entries: &[Entry],
    session_pending_ahead: usize,
    waiter_reason: Option<String>,
) -> QueueEstimate {
    let target_class = run_class(class);
    let per_run = ordinary_per_run_seconds(class);
    let slots = usize::try_from(queue.slots(target_class))
        .unwrap_or(1)
        .max(1);
    let running_higher = entries
        .iter()
        .filter(|entry| entry.running && entry.class < target_class)
        .count();
    let running_same = entries
        .iter()
        .filter(|entry| entry.running && entry.class == target_class)
        .count();
    let waiting_ahead = entries
        .iter()
        .filter(|entry| !entry.running && entry.class <= target_class)
        .count()
        .saturating_add(session_pending_ahead);
    let memory_blocked = !queue.memory_headroom().fits(target_class.budget().memory);
    let reason = waiter_reason.or_else(|| {
        ordinary_queue_reason(
            target_class,
            running_higher,
            running_same,
            slots,
            waiting_ahead,
            memory_blocked,
        )
    });
    let blocking = if running_higher > 0 {
        running_higher.max(slots)
    } else if memory_blocked && running_same < slots {
        slots
    } else {
        running_same
    };
    build_estimate(waiting_ahead, blocking, slots, per_run, reason)
}

fn estimate_utility(session_pending_ahead: usize) -> QueueEstimate {
    let busy = busy_light_slots();
    let reason = if busy >= LIGHT_SLOTS {
        Some(format!(
            "waiting for a free light Rust slot ({LIGHT_SLOTS} slots)"
        ))
    } else if session_pending_ahead > 0 {
        Some(format!(
            "{session_pending_ahead} queued {} ahead in this session",
            if session_pending_ahead == 1 {
                "run"
            } else {
                "runs"
            }
        ))
    } else {
        None
    };
    build_estimate(
        session_pending_ahead,
        busy,
        LIGHT_SLOTS,
        UTILITY_RUN_ESTIMATE_SECS,
        reason,
    )
}

fn project_target_busy_reason(target_count: usize) -> String {
    if target_count <= 1 {
        "waiting for the project's shared Rust target lock".to_owned()
    } else {
        format!("waiting for a free project shared Rust target lock ({target_count} targets)")
    }
}

fn estimate_light(
    queue: Option<&RunQueue>,
    worktree: &Path,
    session_pending_ahead: usize,
) -> QueueEstimate {
    let targets = light_targets(worktree);
    let busy_targets = busy_light_targets(&targets);
    let project_busy = !targets.is_empty() && busy_targets >= targets.len();
    let busy_slots = busy_light_slots();
    let memory_blocked = queue.is_some_and(|queue| !queue.memory_headroom().fits(LIGHT_MEMORY));
    let reason = if project_busy {
        Some(project_target_busy_reason(targets.len()))
    } else if busy_slots >= LIGHT_SLOTS {
        Some(format!(
            "waiting for a free light Rust slot ({LIGHT_SLOTS} slots)"
        ))
    } else if memory_blocked {
        Some("waiting for 3.0 GiB memory headroom above Rosen's reserve".to_owned())
    } else if session_pending_ahead > 0 {
        Some(format!(
            "{session_pending_ahead} queued {} ahead in this session",
            if session_pending_ahead == 1 {
                "run"
            } else {
                "runs"
            }
        ))
    } else {
        None
    };
    let blocking = if project_busy || memory_blocked {
        busy_slots.max(LIGHT_SLOTS)
    } else {
        busy_slots
    };
    build_estimate(
        session_pending_ahead,
        blocking,
        LIGHT_SLOTS,
        LIGHT_RUN_ESTIMATE_SECS,
        reason,
    )
}

fn build_estimate(
    waiting_ahead: usize,
    blocking_running: usize,
    slots: usize,
    per_run_seconds: u64,
    queue_reason: Option<String>,
) -> QueueEstimate {
    let slots = slots.max(1);
    let position = u32::try_from(waiting_ahead.saturating_add(1)).unwrap_or(u32::MAX);
    let free_slots = slots.saturating_sub(blocking_running);
    let estimated_wait_seconds = if waiting_ahead < free_slots && queue_reason.is_none() {
        0
    } else {
        let ahead_total = waiting_ahead.saturating_add(blocking_running);
        let waves = (ahead_total / slots).max(1) as u64;
        waves.saturating_mul(per_run_seconds)
    };
    let estimated_wait = if estimated_wait_seconds == 0 {
        "immediate".to_owned()
    } else {
        format!(
            "~{}",
            format_duration(Duration::from_secs(estimated_wait_seconds))
        )
    };
    QueueEstimate {
        position,
        estimated_wait_seconds,
        estimated_wait,
        queue_reason,
    }
}

/// Acquires a turn for one queued command, cancelling promptly or timing out with a
/// diagnostic report of the queue line and memory.
pub fn acquire(
    request: &QueuedTurnRequest<'_>,
    cancelled: impl Fn() -> bool,
) -> Result<QueuedTurn, QueuedTurnError> {
    acquire_with_progress(request, cancelled, |_| {})
}

/// Like [`acquire`], calling `on_wait` whenever the reason the run is waiting changes.
pub fn acquire_with_progress(
    request: &QueuedTurnRequest<'_>,
    cancelled: impl Fn() -> bool,
    on_wait: impl Fn(&str),
) -> Result<QueuedTurn, QueuedTurnError> {
    acquire_with_ticket(request, cancelled, on_wait, |_| {})
}

/// Like [`acquire_with_progress`], also publishing the live Rosen [`QueueTicket`] as soon
/// as an ordinary run joins Rosen's machine run queue (#665).
pub fn acquire_with_ticket(
    request: &QueuedTurnRequest<'_>,
    cancelled: impl Fn() -> bool,
    on_wait: impl Fn(&str),
    on_ticket: impl Fn(QueueTicket),
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
                request.class,
                &note,
                request.timeout,
                &cancelled,
                &on_wait,
                &on_ticket,
            )
        }
        AgentRun::RustUtility => acquire_utility(&queue, request.timeout, &cancelled, &on_wait),
        AgentRun::RustCheck | AgentRun::Bindings | AgentRun::RegisteredGenerator => acquire_light(
            &queue,
            request.worktree,
            request.directory,
            request.timeout,
            &cancelled,
            &on_wait,
        ),
    }
}

fn report_wait(last_reason: &mut String, reason: &str, on_wait: &impl Fn(&str)) {
    if !reason.is_empty() && last_reason.as_str() != reason {
        last_reason.clear();
        last_reason.push_str(reason);
        on_wait(reason);
    }
}

fn acquire_rosen(
    queue: &RunQueue,
    class: QueuedRunClass,
    note: &str,
    timeout: Duration,
    cancelled: &impl Fn() -> bool,
    on_wait: &impl Fn(&str),
    on_ticket: &impl Fn(QueueTicket),
) -> Result<QueuedTurn, QueuedTurnError> {
    let target_class = run_class(class);
    let waiter = queue.wait(target_class, note)?;
    let ticket = QueueTicket::new(queue.clone(), class, waiter);
    let _clear = ClearTicketOnDrop(ticket.clone());
    on_ticket(ticket.clone());
    let started = Instant::now();
    let mut last_reason = String::new();
    loop {
        if cancelled() {
            return Err(QueuedTurnError::Cancelled);
        }
        let attempt = {
            let guard = ticket.waiter.lock().unwrap_or_else(PoisonError::into_inner);
            match guard.as_ref() {
                Some(waiter) => waiter.try_start()?,
                None => None,
            }
        };
        if let Some(turn) = attempt {
            ticket.clear();
            return Ok(QueuedTurn::Rosen(turn));
        }
        let waiting_for = {
            let guard = ticket.waiter.lock().unwrap_or_else(PoisonError::into_inner);
            guard.as_ref().and_then(Waiter::waiting_for)
        };
        let reason = waiting_for
            .clone()
            .unwrap_or_else(|| format!("waiting for a `{}` run-queue turn", target_class.name()));
        report_wait(&mut last_reason, &reason, on_wait);
        if started.elapsed() >= timeout {
            let waited = started.elapsed();
            ticket.clear();
            return Err(QueuedTurnError::TimedOut(Box::new(
                QueueTimeoutReport::read(queue, target_class.name(), waited, waiting_for),
            )));
        }
        std::thread::sleep(POLL);
    }
}

fn acquire_utility(
    queue: &RunQueue,
    timeout: Duration,
    cancelled: &impl Fn() -> bool,
    on_wait: &impl Fn(&str),
) -> Result<QueuedTurn, QueuedTurnError> {
    let started = Instant::now();
    let reason = format!("waiting for a free light Rust slot ({LIGHT_SLOTS} slots)");
    let mut last_reason = String::new();
    loop {
        if cancelled() {
            return Err(QueuedTurnError::Cancelled);
        }
        if let Some(slot) = try_light_slot()? {
            return Ok(QueuedTurn::Utility { _slot: slot });
        }
        report_wait(&mut last_reason, &reason, on_wait);
        if started.elapsed() >= timeout {
            return Err(QueuedTurnError::TimedOut(Box::new(
                QueueTimeoutReport::read(queue, "light-rust", started.elapsed(), Some(reason)),
            )));
        }
        std::thread::sleep(POLL);
    }
}

fn acquire_light(
    queue: &RunQueue,
    worktree: &Path,
    directory: &Path,
    timeout: Duration,
    cancelled: &impl Fn() -> bool,
    on_wait: &impl Fn(&str),
) -> Result<QueuedTurn, QueuedTurnError> {
    let targets = light_targets(worktree);
    if targets.is_empty() {
        return Err(QueuedTurnError::Io(io::Error::other(
            "no light Rust target directory configured",
        )));
    }
    let identity = crate::light_target::workspace_identity(worktree);
    let started = Instant::now();
    let target_reason = project_target_busy_reason(targets.len());
    let (target, project) = wait_step(
        queue,
        started,
        timeout,
        cancelled,
        on_wait,
        &target_reason,
        || try_lock_light_target(&targets, &identity),
    )?;
    let slot = wait_step(
        queue,
        started,
        timeout,
        cancelled,
        on_wait,
        &format!("waiting for a free light Rust slot ({LIGHT_SLOTS} slots)"),
        try_light_slot,
    )?;
    wait_step(
        queue,
        started,
        timeout,
        cancelled,
        on_wait,
        "waiting for 3.0 GiB memory headroom above Rosen's reserve",
        || Ok(queue.memory_headroom().fits(LIGHT_MEMORY).then_some(())),
    )?;
    crate::light_target::prepare(&target, worktree, directory)?;
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
    on_wait: &impl Fn(&str),
    reason: &str,
    mut attempt: impl FnMut() -> io::Result<Option<T>>,
) -> Result<T, QueuedTurnError> {
    let mut last_reason = String::new();
    loop {
        if cancelled() {
            return Err(QueuedTurnError::Cancelled);
        }
        if let Some(value) = attempt()? {
            return Ok(value);
        }
        report_wait(&mut last_reason, reason, on_wait);
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

    // Risk (#536): queue_run must report accurate queue position, wait estimate and wait reason
    // when slots are busy or other runs are ahead in line.
    #[test]
    fn estimate_reports_position_and_wait_when_slot_busy() -> Result<(), Box<dyn std::error::Error>>
    {
        let queue_dir = tempfile::tempdir()?;
        let queue = RunQueue::at(queue_dir.path());
        queue.set_slots(RunClass::Build, Some(1))?;
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(queue_dir.path().join("run-build.lock"))?;
        holder.lock()?;
        let _waiter = queue.wait(RunClass::Build, "earlier run")?;

        let estimated = estimate(
            AgentRun::Ordinary,
            QueuedRunClass::Build,
            queue_dir.path(),
            Some(queue_dir.path()),
            0,
        );
        drop(holder);
        assert_eq!(estimated.position, 2);
        assert!(estimated.estimated_wait_seconds > 0);
        assert!(
            estimated
                .queue_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("build slot")),
            "{estimated:?}"
        );
        Ok(())
    }

    // Risk (#665): status for an ordinary run already holding a Rosen waiting ticket must
    // report its ticket's actual place via `Waiter::ahead`, without counting itself or later
    // same-class waiters ahead of it.
    #[test]
    fn ticket_estimate_excludes_self_and_later_waiters() -> Result<(), Box<dyn std::error::Error>> {
        let queue_dir = tempfile::tempdir()?;
        let queue = RunQueue::at(queue_dir.path());
        queue.set_slots(RunClass::Build, Some(1))?;

        let first = QueueTicket::new(
            queue.clone(),
            QueuedRunClass::Build,
            queue.wait(RunClass::Build, "first run")?,
        );
        let first_alone = first.estimate().ok_or("expected active ticket estimate")?;
        assert_eq!(
            first_alone.position, 1,
            "the only waiting ticket must not count itself ahead: {first_alone:?}"
        );

        let second = QueueTicket::new(
            queue.clone(),
            QueuedRunClass::Build,
            queue.wait(RunClass::Build, "second run")?,
        );
        let first_with_later = first.estimate().ok_or("expected active ticket estimate")?;
        let second_behind = second.estimate().ok_or("expected active ticket estimate")?;
        assert_eq!(
            first_with_later.position, 1,
            "a later same-class waiter must not inflate an earlier ticket's position: {first_with_later:?}"
        );
        assert_eq!(
            second_behind.position, 2,
            "the second same-class waiter must see one ticket ahead: {second_behind:?}"
        );

        let higher = QueueTicket::new(
            queue.clone(),
            QueuedRunClass::Landing,
            queue.wait(RunClass::Landing, "landing check")?,
        );
        assert_eq!(
            higher.estimate().map(|item| item.position),
            Some(1),
            "higher-priority landing ticket precedes build waiters"
        );
        assert_eq!(
            first.estimate().map(|item| item.position),
            Some(2),
            "first build ticket must count the higher-priority landing ticket ahead"
        );
        assert_eq!(
            second.estimate().map(|item| item.position),
            Some(3),
            "second build ticket must count both landing and first build ahead"
        );

        first.clear();
        assert!(!first.is_active());
        assert!(first.estimate().is_none());
        Ok(())
    }

    // Risk (#682): holding one pooled project target lock must not report a project target
    // wait while another target in the project's pool is free; only when every pooled
    // target is locked should estimate_light report a target-lock wait.
    #[test]
    fn estimate_light_reports_project_busy_only_when_all_pooled_targets_are_held()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = tempfile::tempdir()?;
        let workspace = project.path().join("issue-682");
        std::fs::create_dir_all(workspace.join(".rosen"))?;

        let targets = light_targets(&workspace);
        assert_eq!(targets.len(), LIGHT_SLOTS);
        let identity = crate::light_target::workspace_identity(&workspace);

        let (_first_target, first_lock) =
            try_lock_light_target(&targets, &identity)?.ok_or("expected first pooled target")?;
        let one_busy = estimate_light(None, &workspace, 0);
        assert!(
            !one_busy
                .queue_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("shared Rust target lock")),
            "one held pooled target must not block the project: {one_busy:?}"
        );

        let (_second_target, second_lock) =
            try_lock_light_target(&targets, &identity)?.ok_or("expected second pooled target")?;
        let all_busy = estimate_light(None, &workspace, 0);
        drop((first_lock, second_lock));
        assert!(
            all_busy
                .queue_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("shared Rust target lock")),
            "holding every pooled target must report a target-lock wait: {all_busy:?}"
        );
        assert!(all_busy.estimated_wait_seconds > 0);
        Ok(())
    }
}
