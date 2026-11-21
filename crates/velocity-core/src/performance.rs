//! Runtime performance budgets, rolling measurements and breach detection (#557).
//!
//! Pure synchronous aggregation lives here so `velocity-store`, `velocity-github` and
//! `velocity-app` can report into one lightweight collector, emit structured breach lines
//! to `velocity.log`, and surface current health in the diagnostics view.

use std::collections::{BTreeMap, VecDeque};
use std::panic::Location;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Budget for UI command p95 latency (strictly under 200 ms).
pub const UI_COMMAND_P95_BUDGET_MS: u32 = 200;
/// Budget for persisted store lock hold duration (strictly under 20 ms).
pub const STORE_LOCK_HOLD_BUDGET_MS: u32 = 20;
/// Budget for store lock time spent inside one `state.json` save (strictly under 10 ms).
pub const SAVE_LOCK_BUDGET_MS: u32 = 10;
/// Budget for `gh` calls per minute per project at steady state (3 or fewer).
pub const GH_CALLS_PER_MINUTE_BUDGET: u32 = 3;
/// Startup window before steady-state `gh` rate breaches are enforced.
pub const GH_STARTUP_GRACE_SECONDS: u64 = 60;
/// Source tag written to `velocity.log` when a performance budget is breached.
pub const BREACH_LOG_SOURCE: &str = "perf-budget";

const WINDOW_SAMPLES: usize = 128;
const MAX_RECENT_BREACHES: usize = 50;
const LOG_COOLDOWN: Duration = Duration::from_secs(10);
const GH_WINDOW: Duration = Duration::from_secs(60);

type BreachLogger = fn(&str);

static BREACH_LOGGER: OnceLock<BreachLogger> = OnceLock::new();
static TRACKER: LazyLock<Mutex<PerformanceTracker>> =
    LazyLock::new(|| Mutex::new(PerformanceTracker::new(Instant::now())));

/// Registers the sink (such as `diagnostics::record`) called on budget breaches.
/// The callback always runs after the tracker lock is released.
pub fn set_breach_logger(logger: BreachLogger) {
    let _ = BREACH_LOGGER.set(logger);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum BudgetCategory {
    UiCommand,
    StoreLock,
    StateSave,
    GhRate,
}

impl BudgetCategory {
    pub fn as_tag(self) -> &'static str {
        match self {
            Self::UiCommand => "ui_command",
            Self::StoreLock => "store_lock",
            Self::StateSave => "state_save",
            Self::GhRate => "gh_rate",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct PerformanceBudgets {
    pub ui_command_p95_ms: u32,
    pub store_lock_hold_ms: u32,
    pub save_lock_ms: u32,
    pub gh_calls_per_minute: u32,
}

impl Default for PerformanceBudgets {
    fn default() -> Self {
        Self {
            ui_command_p95_ms: UI_COMMAND_P95_BUDGET_MS,
            store_lock_hold_ms: STORE_LOCK_HOLD_BUDGET_MS,
            save_lock_ms: SAVE_LOCK_BUDGET_MS,
            gh_calls_per_minute: GH_CALLS_PER_MINUTE_BUDGET,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct CommandLatencyRow {
    pub command: String,
    pub calls: u32,
    pub p95_ms: u32,
    pub max_ms: u32,
    pub last_ms: u32,
    pub breaches: u32,
    pub breaching: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct StoreLockRow {
    pub caller: String,
    pub acquisitions: u32,
    pub max_wait_ms: u32,
    pub last_wait_ms: u32,
    pub p95_hold_ms: u32,
    pub max_hold_ms: u32,
    pub last_hold_ms: u32,
    pub breaches: u32,
    pub breaching: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct SaveDiagnostics {
    pub count: u32,
    pub last_duration_ms: u32,
    pub p95_duration_ms: u32,
    pub max_duration_ms: u32,
    pub last_lock_ms: u32,
    pub p95_lock_ms: u32,
    pub max_lock_ms: u32,
    pub breaches: u32,
    pub breaching: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct GhRateRow {
    pub project: String,
    pub calls_last_minute: u32,
    pub peak_per_minute: u32,
    pub total_calls: u32,
    pub breaches: u32,
    pub breaching: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct BudgetBreach {
    pub unix_ms: String,
    pub category: BudgetCategory,
    pub subject: String,
    pub observed: u32,
    pub budget: u32,
    pub unit: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct PerformanceDiagnostics {
    pub uptime_seconds: u32,
    pub steady_state: bool,
    pub budgets: PerformanceBudgets,
    pub total_breaches: u32,
    pub ui_commands: Vec<CommandLatencyRow>,
    pub store_locks: Vec<StoreLockRow>,
    pub saves: SaveDiagnostics,
    pub gh_projects: Vec<GhRateRow>,
    pub recent_breaches: Vec<BudgetBreach>,
}

#[derive(Default)]
struct CommandSeries {
    calls: u32,
    samples_ms: VecDeque<u32>,
    max_ms: u32,
    last_ms: u32,
    breaches: u32,
}

#[derive(Default)]
struct LockSeries {
    acquisitions: u32,
    hold_samples_ms: VecDeque<u32>,
    max_wait_ms: u32,
    last_wait_ms: u32,
    max_hold_ms: u32,
    last_hold_ms: u32,
    breaches: u32,
}

#[derive(Default)]
struct SaveSeries {
    count: u32,
    duration_samples_ms: VecDeque<u32>,
    lock_samples_ms: VecDeque<u32>,
    last_duration_ms: u32,
    max_duration_ms: u32,
    last_lock_ms: u32,
    max_lock_ms: u32,
    breaches: u32,
}

#[derive(Default)]
struct GhSeries {
    timestamps: VecDeque<Instant>,
    peak_per_minute: u32,
    total_calls: u32,
    breaches: u32,
}

/// Pure, deterministic performance telemetry collector.
pub struct PerformanceTracker {
    started_at: Instant,
    commands: BTreeMap<String, CommandSeries>,
    store_locks: BTreeMap<String, LockSeries>,
    saves: SaveSeries,
    gh_projects: BTreeMap<String, GhSeries>,
    recent_breaches: VecDeque<BudgetBreach>,
    total_breaches: u32,
    last_logged: BTreeMap<(BudgetCategory, String), (Instant, u32)>,
}

impl PerformanceTracker {
    pub fn new(started_at: Instant) -> Self {
        Self {
            started_at,
            commands: BTreeMap::new(),
            store_locks: BTreeMap::new(),
            saves: SaveSeries::default(),
            gh_projects: BTreeMap::new(),
            recent_breaches: VecDeque::new(),
            total_breaches: 0,
            last_logged: BTreeMap::new(),
        }
    }

    pub fn record_ui_command(
        &mut self,
        command: &str,
        duration: Duration,
        now: Instant,
        unix_ms: u64,
    ) -> Option<String> {
        let ms = duration_ms(duration);
        let entry = self.commands.entry(command.to_owned()).or_default();
        entry.calls = entry.calls.saturating_add(1);
        entry.last_ms = ms;
        entry.max_ms = entry.max_ms.max(ms);
        push_bounded(&mut entry.samples_ms, ms, WINDOW_SAMPLES);
        let p95 = percentile_95(&entry.samples_ms);
        if p95 < UI_COMMAND_P95_BUDGET_MS {
            return None;
        }
        entry.breaches = entry.breaches.saturating_add(1);
        let detail = format!(
            "p95_ms={p95} budget_ms={UI_COMMAND_P95_BUDGET_MS} last_ms={ms} max_ms={} calls={}",
            entry.max_ms, entry.calls
        );
        self.record_breach(
            BudgetCategory::UiCommand,
            command,
            p95,
            UI_COMMAND_P95_BUDGET_MS,
            "ms",
            detail,
            now,
            unix_ms,
        )
    }

    pub fn record_store_lock(
        &mut self,
        caller: &str,
        wait: Duration,
        hold: Duration,
        now: Instant,
        unix_ms: u64,
    ) -> Option<String> {
        let wait_ms = duration_ms(wait);
        let hold_ms = duration_ms(hold);
        let entry = self.store_locks.entry(caller.to_owned()).or_default();
        entry.acquisitions = entry.acquisitions.saturating_add(1);
        entry.last_wait_ms = wait_ms;
        entry.max_wait_ms = entry.max_wait_ms.max(wait_ms);
        entry.last_hold_ms = hold_ms;
        entry.max_hold_ms = entry.max_hold_ms.max(hold_ms);
        push_bounded(&mut entry.hold_samples_ms, hold_ms, WINDOW_SAMPLES);
        if hold_ms < STORE_LOCK_HOLD_BUDGET_MS {
            return None;
        }
        entry.breaches = entry.breaches.saturating_add(1);
        let detail = format!(
            "hold_ms={hold_ms} budget_ms={STORE_LOCK_HOLD_BUDGET_MS} wait_ms={wait_ms} max_hold_ms={} acquisitions={}",
            entry.max_hold_ms, entry.acquisitions
        );
        self.record_breach(
            BudgetCategory::StoreLock,
            caller,
            hold_ms,
            STORE_LOCK_HOLD_BUDGET_MS,
            "ms",
            detail,
            now,
            unix_ms,
        )
    }

    pub fn record_save(
        &mut self,
        duration: Duration,
        lock_time: Duration,
        now: Instant,
        unix_ms: u64,
    ) -> Option<String> {
        let total_ms = duration_ms(duration);
        let lock_ms = duration_ms(lock_time);
        self.saves.count = self.saves.count.saturating_add(1);
        self.saves.last_duration_ms = total_ms;
        self.saves.max_duration_ms = self.saves.max_duration_ms.max(total_ms);
        self.saves.last_lock_ms = lock_ms;
        self.saves.max_lock_ms = self.saves.max_lock_ms.max(lock_ms);
        push_bounded(
            &mut self.saves.duration_samples_ms,
            total_ms,
            WINDOW_SAMPLES,
        );
        push_bounded(&mut self.saves.lock_samples_ms, lock_ms, WINDOW_SAMPLES);
        if lock_ms < SAVE_LOCK_BUDGET_MS {
            return None;
        }
        self.saves.breaches = self.saves.breaches.saturating_add(1);
        let detail = format!(
            "lock_ms={lock_ms} budget_ms={SAVE_LOCK_BUDGET_MS} duration_ms={total_ms} max_lock_ms={} saves={}",
            self.saves.max_lock_ms, self.saves.count
        );
        self.record_breach(
            BudgetCategory::StateSave,
            "state.json",
            lock_ms,
            SAVE_LOCK_BUDGET_MS,
            "ms",
            detail,
            now,
            unix_ms,
        )
    }

    pub fn record_gh_call(
        &mut self,
        project: &str,
        caller: &str,
        operation: &str,
        now: Instant,
        unix_ms: u64,
    ) -> Option<String> {
        let key = normalize_github_project(project);
        let steady = self.is_steady_state(now);
        let entry = self.gh_projects.entry(key.clone()).or_default();
        prune_gh_window(&mut entry.timestamps, now);
        entry.timestamps.push_back(now);
        entry.total_calls = entry.total_calls.saturating_add(1);
        let rate = u32::try_from(entry.timestamps.len()).unwrap_or(u32::MAX);
        entry.peak_per_minute = entry.peak_per_minute.max(rate);
        if !steady || rate <= GH_CALLS_PER_MINUTE_BUDGET {
            return None;
        }
        entry.breaches = entry.breaches.saturating_add(1);
        let detail = format!(
            "calls_per_min={rate} budget={GH_CALLS_PER_MINUTE_BUDGET} total={} caller={caller} op={operation}",
            entry.total_calls
        );
        self.record_breach(
            BudgetCategory::GhRate,
            &key,
            rate,
            GH_CALLS_PER_MINUTE_BUDGET,
            "calls/min",
            detail,
            now,
            unix_ms,
        )
    }

    pub fn snapshot(&mut self, now: Instant) -> PerformanceDiagnostics {
        let uptime = now.saturating_duration_since(self.started_at);
        let steady_state = self.is_steady_state(now);
        PerformanceDiagnostics {
            uptime_seconds: u32::try_from(uptime.as_secs()).unwrap_or(u32::MAX),
            steady_state,
            budgets: PerformanceBudgets::default(),
            total_breaches: self.total_breaches,
            ui_commands: self.command_rows(),
            store_locks: self.lock_rows(),
            saves: self.save_diagnostics(),
            gh_projects: self.gh_rows(now, steady_state),
            recent_breaches: self.recent_breaches.iter().cloned().collect(),
        }
    }

    fn is_steady_state(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started_at).as_secs() >= GH_STARTUP_GRACE_SECONDS
    }

    #[allow(clippy::too_many_arguments)]
    fn record_breach(
        &mut self,
        category: BudgetCategory,
        subject: &str,
        observed: u32,
        budget: u32,
        unit: &str,
        detail: String,
        now: Instant,
        unix_ms: u64,
    ) -> Option<String> {
        self.total_breaches = self.total_breaches.saturating_add(1);
        if self.recent_breaches.len() >= MAX_RECENT_BREACHES {
            self.recent_breaches.pop_back();
        }
        self.recent_breaches.push_front(BudgetBreach {
            unix_ms: unix_ms.to_string(),
            category,
            subject: subject.to_owned(),
            observed,
            budget,
            unit: unit.to_owned(),
            detail: detail.clone(),
        });
        let key = (category, subject.to_owned());
        let should_log = match self.last_logged.get(&key) {
            None => true,
            Some((last_at, peak)) => {
                now.saturating_duration_since(*last_at) >= LOG_COOLDOWN || observed > *peak
            }
        };
        if !should_log {
            return None;
        }
        self.last_logged.insert(key, (now, observed));
        Some(format!("{} subject={subject} {detail}", category.as_tag()))
    }

    fn command_rows(&self) -> Vec<CommandLatencyRow> {
        let mut rows: Vec<_> = self
            .commands
            .iter()
            .map(|(command, series)| {
                let p95_ms = percentile_95(&series.samples_ms);
                CommandLatencyRow {
                    command: command.clone(),
                    calls: series.calls,
                    p95_ms,
                    max_ms: series.max_ms,
                    last_ms: series.last_ms,
                    breaches: series.breaches,
                    breaching: p95_ms >= UI_COMMAND_P95_BUDGET_MS,
                }
            })
            .collect();
        rows.sort_by(|left, right| {
            right
                .breaching
                .cmp(&left.breaching)
                .then(right.p95_ms.cmp(&left.p95_ms))
                .then(right.max_ms.cmp(&left.max_ms))
                .then(left.command.cmp(&right.command))
        });
        rows
    }

    fn lock_rows(&self) -> Vec<StoreLockRow> {
        let mut rows: Vec<_> = self
            .store_locks
            .iter()
            .map(|(caller, series)| StoreLockRow {
                caller: caller.clone(),
                acquisitions: series.acquisitions,
                max_wait_ms: series.max_wait_ms,
                last_wait_ms: series.last_wait_ms,
                p95_hold_ms: percentile_95(&series.hold_samples_ms),
                max_hold_ms: series.max_hold_ms,
                last_hold_ms: series.last_hold_ms,
                breaches: series.breaches,
                breaching: series.breaches > 0,
            })
            .collect();
        rows.sort_by(|left, right| {
            right
                .breaching
                .cmp(&left.breaching)
                .then(right.max_hold_ms.cmp(&left.max_hold_ms))
                .then(right.max_wait_ms.cmp(&left.max_wait_ms))
                .then(left.caller.cmp(&right.caller))
        });
        rows
    }

    fn save_diagnostics(&self) -> SaveDiagnostics {
        SaveDiagnostics {
            count: self.saves.count,
            last_duration_ms: self.saves.last_duration_ms,
            p95_duration_ms: percentile_95(&self.saves.duration_samples_ms),
            max_duration_ms: self.saves.max_duration_ms,
            last_lock_ms: self.saves.last_lock_ms,
            p95_lock_ms: percentile_95(&self.saves.lock_samples_ms),
            max_lock_ms: self.saves.max_lock_ms,
            breaches: self.saves.breaches,
            breaching: self.saves.breaches > 0,
        }
    }

    fn gh_rows(&mut self, now: Instant, steady_state: bool) -> Vec<GhRateRow> {
        let mut rows: Vec<_> = self
            .gh_projects
            .iter_mut()
            .map(|(project, series)| {
                prune_gh_window(&mut series.timestamps, now);
                let calls_last_minute = u32::try_from(series.timestamps.len()).unwrap_or(u32::MAX);
                let breaching = (steady_state && calls_last_minute > GH_CALLS_PER_MINUTE_BUDGET)
                    || series.breaches > 0;
                GhRateRow {
                    project: project.clone(),
                    calls_last_minute,
                    peak_per_minute: series.peak_per_minute,
                    total_calls: series.total_calls,
                    breaches: series.breaches,
                    breaching,
                }
            })
            .collect();
        rows.sort_by(|left, right| {
            right
                .breaching
                .cmp(&left.breaching)
                .then(right.calls_last_minute.cmp(&left.calls_last_minute))
                .then(right.total_calls.cmp(&left.total_calls))
                .then(left.project.cmp(&right.project))
        });
        rows
    }
}

/// Formats a Rust caller location as the last two path segments plus line number.
pub fn caller_label(caller: &Location<'static>) -> String {
    let parts: Vec<&str> = caller.file().split(['/', '\\']).collect();
    let file = parts[parts.len().saturating_sub(2)..].join("/");
    format!("{file}:{}", caller.line())
}

/// Normalizes a repository name or GitHub remote URL into a canonical `owner/repo` key.
pub fn normalize_github_project(origin: &str) -> String {
    let trimmed = origin.trim().trim_end_matches('/').trim_end_matches(".git");
    if trimmed.is_empty() {
        return "(global)".to_owned();
    }
    let candidate = if let Some((_, after)) = trimmed.split_once("github.com/") {
        after
    } else if let Some((_, after)) = trimmed.split_once("github.com:") {
        after
    } else {
        trimmed
    };
    let mut parts = candidate.split('/').filter(|segment| !segment.is_empty());
    if let (Some(owner), Some(repo)) = (parts.next(), parts.next()) {
        return format!("{owner}/{repo}").to_ascii_lowercase();
    }
    trimmed.to_ascii_lowercase()
}

pub fn record_ui_command(command: &str, duration: Duration) {
    let log_line = {
        let mut tracker = lock_tracker();
        tracker.record_ui_command(command, duration, Instant::now(), unix_now_ms())
    };
    emit_breach(log_line);
}

pub fn record_store_lock(caller: &Location<'static>, wait: Duration, hold: Duration) {
    let label = caller_label(caller);
    let log_line = {
        let mut tracker = lock_tracker();
        tracker.record_store_lock(&label, wait, hold, Instant::now(), unix_now_ms())
    };
    emit_breach(log_line);
}

pub fn record_save(duration: Duration, lock_time: Duration) {
    let log_line = {
        let mut tracker = lock_tracker();
        tracker.record_save(duration, lock_time, Instant::now(), unix_now_ms())
    };
    emit_breach(log_line);
}

pub fn record_gh_call(project: &str, caller: &Location<'static>, operation: &str) {
    let label = caller_label(caller);
    let log_line = {
        let mut tracker = lock_tracker();
        tracker.record_gh_call(project, &label, operation, Instant::now(), unix_now_ms())
    };
    emit_breach(log_line);
}

pub fn snapshot() -> PerformanceDiagnostics {
    let mut tracker = lock_tracker();
    tracker.snapshot(Instant::now())
}

fn lock_tracker() -> std::sync::MutexGuard<'static, PerformanceTracker> {
    TRACKER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn emit_breach(message: Option<String>) {
    if let (Some(line), Some(logger)) = (message, BREACH_LOGGER.get()) {
        logger(&line);
    }
}

fn duration_ms(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

fn unix_now_ms() -> u64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

fn push_bounded(samples: &mut VecDeque<u32>, value: u32, capacity: usize) {
    if samples.len() >= capacity {
        samples.pop_front();
    }
    samples.push_back(value);
}

fn prune_gh_window(timestamps: &mut VecDeque<Instant>, now: Instant) {
    while let Some(oldest) = timestamps.front() {
        if now.saturating_duration_since(*oldest) > GH_WINDOW {
            timestamps.pop_front();
        } else {
            break;
        }
    }
}

fn percentile_95(samples: &VecDeque<u32>) -> u32 {
    if samples.is_empty() {
        return 0;
    }
    let mut sorted: Vec<u32> = samples.iter().copied().collect();
    sorted.sort_unstable();
    let index = (sorted.len() * 95)
        .div_ceil(100)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[index]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#557): performance budgets must detect breaches accurately for p95 UI latency,
    // store lock holds, save lock time and steady-state per-project gh call rate.
    #[test]
    fn detects_ui_command_p95_budget_breaches() {
        let start = Instant::now();
        let mut tracker = PerformanceTracker::new(start);
        for index in 0..19 {
            assert!(
                tracker
                    .record_ui_command("switch_project", Duration::from_millis(25), start, index)
                    .is_none()
            );
        }
        // One outlier out of 20 keeps p95 at 25 ms (within the 200 ms budget).
        assert!(
            tracker
                .record_ui_command("switch_project", Duration::from_millis(260), start, 20)
                .is_none()
        );
        // A second slow call pushes p95 to 260 ms and triggers a logged breach.
        let breach =
            tracker.record_ui_command("switch_project", Duration::from_millis(275), start, 21);
        assert!(breach.is_some());
        let snap = tracker.snapshot(start);
        assert_eq!(snap.total_breaches, 1);
        assert_eq!(snap.ui_commands[0].command, "switch_project");
        assert!(snap.ui_commands[0].breaching);
        assert!(snap.ui_commands[0].p95_ms >= UI_COMMAND_P95_BUDGET_MS);
    }

    #[test]
    fn detects_store_lock_and_save_lock_breaches() {
        let start = Instant::now();
        let mut tracker = PerformanceTracker::new(start);
        assert!(
            tracker
                .record_store_lock(
                    "src/projects.rs:49",
                    Duration::from_millis(2),
                    Duration::from_millis(19),
                    start,
                    1,
                )
                .is_none()
        );
        let lock_breach = tracker.record_store_lock(
            "src/projects.rs:49",
            Duration::from_millis(7),
            Duration::from_millis(24),
            start,
            2,
        );
        assert!(lock_breach.is_some());

        assert!(
            tracker
                .record_save(
                    Duration::from_millis(50),
                    Duration::from_millis(9),
                    start,
                    3
                )
                .is_none()
        );
        let save_breach = tracker.record_save(
            Duration::from_millis(85),
            Duration::from_millis(12),
            start,
            4,
        );
        assert!(save_breach.is_some());

        let snap = tracker.snapshot(start);
        assert_eq!(snap.total_breaches, 2);
        assert!(snap.store_locks[0].breaching);
        assert_eq!(snap.store_locks[0].max_wait_ms, 7);
        assert_eq!(snap.store_locks[0].max_hold_ms, 24);
        assert!(snap.saves.breaching);
        assert_eq!(snap.saves.max_lock_ms, 12);
        assert_eq!(snap.saves.max_duration_ms, 85);
    }

    #[test]
    fn enforces_gh_per_project_rate_only_at_steady_state() {
        let start = Instant::now();
        let mut tracker = PerformanceTracker::new(start);
        // Startup bursts inside the first 60s are measured without breaching steady state.
        for index in 0..5 {
            assert!(
                tracker
                    .record_gh_call(
                        "https://github.com/mESmaC/velocity.git",
                        "issue_panel.rs:48",
                        "graphql issues",
                        start + Duration::from_secs(index),
                        index,
                    )
                    .is_none()
            );
        }
        let steady = start + Duration::from_secs(GH_STARTUP_GRACE_SECONDS + 70);
        for index in 0..3 {
            assert!(
                tracker
                    .record_gh_call(
                        "mESmaC/velocity",
                        "issue_panel.rs:48",
                        "graphql issues",
                        steady + Duration::from_secs(index),
                        100 + index,
                    )
                    .is_none()
            );
        }
        let breach = tracker.record_gh_call(
            "git@github.com:mESmaC/velocity.git",
            "project_health.rs:1210",
            "api GET",
            steady + Duration::from_secs(10),
            110,
        );
        assert!(breach.is_some());
        let snap = tracker.snapshot(steady + Duration::from_secs(10));
        assert!(snap.steady_state);
        assert_eq!(snap.gh_projects.len(), 1);
        assert_eq!(snap.gh_projects[0].project, "mesmac/velocity");
        assert_eq!(snap.gh_projects[0].calls_last_minute, 4);
        assert!(snap.gh_projects[0].breaching);
    }
}
