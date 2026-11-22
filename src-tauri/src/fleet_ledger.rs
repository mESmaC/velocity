//! Fleet development ledger commands, lifecycle hooks, idle slot-minute sampling,
//! and Collapse state-feed hourly rollup delivery (#619).

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use velocity_collapse::FleetRollupFeedItem;
use velocity_core::collapse::{collapse_root, parse_rfc3339_seconds};
use velocity_core::fleet_ledger::{
    FleetCheckRecord, FleetCollapseHourRollup, FleetStatusView, build_window_summary,
};
use velocity_core::queued_runs::{QueuedRunPhase, QueuedRunRecord};
use velocity_github::GitHubIssue;
use velocity_store::fleet_ledger::FleetDispatchInput;
use velocity_threads::ThreadKey;

use crate::state::ManagedStateStore;
use crate::threads::lock;

/// Upper bound on one live dispatch tick's elapsed time so machine sleep or process restart
/// never fabricates idle slot-minutes while Velocity was not running.
const MAX_IDLE_TICK_SECS: u64 = 120;
/// Flush accumulated idle slot-seconds to `StateStore` every 60 seconds during idle spells.
const IDLE_FLUSH_INTERVAL_SECS: u64 = 60;
/// Minimum interval between routine Collapse state-feed fleet-rollup pushes per project.
const COLLAPSE_PUSH_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Send up to 48 hours of non-empty hourly rollups to Collapse's state feed.
const COLLAPSE_PUSH_WINDOW_HOURS: u32 = 48;

#[derive(Clone, Debug)]
struct IdleSampleState {
    last_tick_secs: u64,
    pending_start_secs: Option<u64>,
    pending_end_secs: u64,
    pending_free_slots: u32,
}

#[derive(Clone, Debug)]
struct CollapsePushState {
    last_pushed_at: Instant,
    last_payload_json: String,
}

static IDLE_SAMPLES: OnceLock<Mutex<BTreeMap<u32, IdleSampleState>>> = OnceLock::new();
static COLLAPSE_PUSHES: OnceLock<Mutex<BTreeMap<u32, CollapsePushState>>> = OnceLock::new();

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Returns the Fleet status summary (last 24 hours and last 7 days + top offenders) for `project_identifier`.
#[tauri::command]
#[specta::specta]
pub async fn get_fleet_stats(
    application: AppHandle,
    project_identifier: u32,
) -> Result<FleetStatusView, String> {
    let now = unix_now();
    crate::state::with_store(&application, move |store| {
        store
            .fleet_status_view(project_identifier, now)
            .unwrap_or_default()
    })
    .await
}

/// Records an issue dispatch attempt (automatic or manual).
pub(crate) fn record_dispatch(
    application: &AppHandle,
    project: u32,
    repository: &str,
    issue: &GitHubIssue,
    thread_id: u32,
    provider: &str,
    model: &str,
) {
    let now = unix_now();
    let filed_at = parse_rfc3339_seconds(&issue.created_at);
    let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let _ = store.record_fleet_dispatch(
        project,
        FleetDispatchInput {
            repository,
            issue: issue.number,
            title: Some(issue.title.as_str()),
            url: Some(issue.url.as_str()),
            filed_at,
            thread_id,
            provider,
            model,
            now_secs: now,
        },
    );
}

/// Binds a newly created Rosen change ID to the thread's issue ledger record.
pub(crate) fn bind_change(application: &AppHandle, key: ThreadKey, change: &str) {
    let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let _ = store.bind_fleet_change(key.0, key.1, change);
}

pub(crate) fn record_snapshot(application: &AppHandle, project: u32, change: &str) {
    let now = unix_now();
    let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let _ = store.record_fleet_snapshot(project, change, now);
}

pub(crate) fn record_named(application: &AppHandle, project: u32, change: &str) {
    let now = unix_now();
    let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let _ = store.record_fleet_named(project, change, now);
}

pub(crate) fn record_landed(
    application: &AppHandle,
    project: u32,
    change: &str,
    commit: Option<&str>,
) {
    let now = unix_now();
    let changed = {
        let Ok(mut store) = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        ) else {
            return;
        };
        store
            .record_fleet_landed(project, change, commit, now)
            .unwrap_or(false)
    };
    if changed {
        push_collapse_rollups_if_due(application, project, true);
    }
}

pub(crate) fn record_post_land_green(
    application: &AppHandle,
    project: u32,
    change: &str,
    commit: &str,
) {
    let now = unix_now();
    let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let _ = store.record_fleet_post_land_green(project, change, commit, now);
}

pub(crate) fn record_failure_or_revert(
    application: &AppHandle,
    project: u32,
    change: Option<&str>,
    commit: Option<&str>,
    event_seq: u64,
    is_revert: bool,
) {
    let now = unix_now();
    let changed = {
        let Ok(mut store) = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        ) else {
            return;
        };
        store
            .record_fleet_failure_or_revert(project, change, commit, event_seq, is_revert, now)
            .unwrap_or(false)
    };
    if changed {
        push_collapse_rollups_if_due(application, project, false);
    }
}

pub(crate) fn record_closed(
    application: &AppHandle,
    project: u32,
    change: &str,
    issue: Option<(&str, u32)>,
) {
    let now = unix_now();
    let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let _ = store.record_fleet_closed(project, change, issue, now);
}

pub(crate) fn record_turn(
    application: &AppHandle,
    key: ThreadKey,
    tokens_in: u64,
    tokens_out: u64,
    wall_ms: u64,
) {
    let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let _ = store.record_fleet_turn(key.0, key.1, tokens_in, tokens_out, wall_ms);
}

pub(crate) fn record_check(application: &AppHandle, key: ThreadKey, record: &QueuedRunRecord) {
    let check = FleetCheckRecord {
        id: record.id.clone(),
        command: record.command_line(),
        queued_at: record.queued_at.unwrap_or_else(unix_now),
        wait_ms: record.wait_ms,
        run_ms: record.run_ms,
        succeeded: record.phase == QueuedRunPhase::Completed,
    };
    let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let _ = store.record_fleet_check(key.0, key.1, check);
}

pub(crate) fn record_restart(application: &AppHandle, key: ThreadKey) {
    let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let _ = store.record_fleet_restart(key.0, key.1);
}

pub(crate) fn record_review_finding(
    application: &AppHandle,
    project: u32,
    change: &str,
    target: &str,
) {
    let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let _ = store.record_fleet_review_finding(project, change, target);
}

/// Samples idle slots (`slots - busy` when dispatch is active and unpaused) and flushes
/// accumulated idle slot-seconds to `StateStore` every minute or when slot occupancy changes.
pub(crate) fn observe_idle_slots(
    application: &AppHandle,
    project: u32,
    slots: u32,
    busy: u32,
    paused: bool,
    now_secs: u64,
) {
    let free_slots = if paused || slots == 0 {
        0
    } else {
        slots.saturating_sub(busy)
    };
    let to_flush = sample_idle_intervals(project, free_slots, now_secs);
    if to_flush.is_empty() {
        return;
    }
    let flushed = {
        let Ok(mut store) = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        ) else {
            return;
        };
        let mut any = false;
        for (start, end, free) in to_flush {
            if store
                .record_fleet_idle_interval(project, start, end, free)
                .unwrap_or(false)
            {
                any = true;
            }
        }
        any
    };
    if flushed {
        push_collapse_rollups_if_due(application, project, false);
    }
}

fn sample_idle_intervals(project: u32, free_slots: u32, now_secs: u64) -> Vec<(u64, u64, u32)> {
    let mut to_flush = Vec::new();
    let map = IDLE_SAMPLES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let Ok(mut guard) = map.lock() else {
        return to_flush;
    };
    let state = guard.entry(project).or_insert(IdleSampleState {
        last_tick_secs: now_secs,
        pending_start_secs: None,
        pending_end_secs: now_secs,
        pending_free_slots: 0,
    });
    let delta = now_secs.saturating_sub(state.last_tick_secs);
    let prev_tick = state.last_tick_secs;
    state.last_tick_secs = now_secs;
    if delta > 0 && delta <= MAX_IDLE_TICK_SECS {
        if state.pending_free_slots != free_slots {
            if let Some(start) = state.pending_start_secs.take()
                && state.pending_free_slots > 0
                && state.pending_end_secs > start
            {
                to_flush.push((start, state.pending_end_secs, state.pending_free_slots));
            }
            state.pending_free_slots = free_slots;
            if free_slots > 0 {
                state.pending_start_secs = Some(prev_tick);
                state.pending_end_secs = now_secs;
            }
        } else if free_slots > 0 {
            if state.pending_start_secs.is_none() {
                state.pending_start_secs = Some(prev_tick);
            }
            state.pending_end_secs = now_secs;
        }
        if let Some(start) = state.pending_start_secs
            && state.pending_end_secs.saturating_sub(start) >= IDLE_FLUSH_INTERVAL_SECS
        {
            to_flush.push((start, state.pending_end_secs, state.pending_free_slots));
            state.pending_start_secs = Some(state.pending_end_secs);
        }
    } else if let Some(start) = state.pending_start_secs.take() {
        if state.pending_free_slots > 0 && state.pending_end_secs > start {
            to_flush.push((start, state.pending_end_secs, state.pending_free_slots));
        }
        state.pending_free_slots = free_slots;
    }
    to_flush
}

/// Sends hourly fleet rollups (counts and durations only) to Collapse's state feed when the
/// project has live Collapse state enabled and rollups changed since the last push (#619).
pub(crate) fn push_collapse_rollups_if_due(
    application: &AppHandle,
    project: u32,
    force_immediate: bool,
) {
    let Some((root, rollups, digest)) = prepare_collapse_rollups(application, project) else {
        return;
    };
    if !should_push_collapse_rollups(project, &digest, force_immediate) {
        return;
    }
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        let feed_id = handle
            .path()
            .app_data_dir()
            .ok()
            .map(|data| velocity_rosen::ProjectServer::new(&data, project))
            .and_then(|server| velocity_rosen::collapse_link::collapse_feed_id(&server));
        match velocity_collapse::push_fleet_rollups(&root, feed_id.as_deref(), &rollups).await {
            Ok(true) => {
                let map = COLLAPSE_PUSHES.get_or_init(|| Mutex::new(BTreeMap::new()));
                if let Ok(mut guard) = map.lock() {
                    guard.insert(
                        project,
                        CollapsePushState {
                            last_pushed_at: Instant::now(),
                            last_payload_json: digest,
                        },
                    );
                }
            }
            Ok(false) => {}
            Err(error) => {
                crate::diagnostics::record(
                    "fleet ledger",
                    &format!(
                        "project {project}: could not push fleet rollups to Collapse: {error}"
                    ),
                );
            }
        }
    });
}

fn prepare_collapse_rollups(
    application: &AppHandle,
    project: u32,
) -> Option<(String, Vec<FleetRollupFeedItem>, String)> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    let item = store.project(project)?;
    if !item.collapse_live_state {
        return None;
    }
    let root = collapse_root(item.collapse_repository.as_deref()?)?;
    let summary = build_window_summary(&item.fleet_ledger, COLLAPSE_PUSH_WINDOW_HOURS, unix_now());
    let rollups: Vec<FleetRollupFeedItem> = summary
        .hours
        .iter()
        .map(FleetCollapseHourRollup::from)
        .map(into_collapse_item)
        .collect();
    if rollups.is_empty() {
        return None;
    }
    let digest = serde_json::to_string(&rollups).ok()?;
    Some((root, rollups, digest))
}

fn should_push_collapse_rollups(project: u32, digest: &str, force_immediate: bool) -> bool {
    let map = COLLAPSE_PUSHES.get_or_init(|| Mutex::new(BTreeMap::new()));
    let Ok(guard) = map.lock() else {
        return false;
    };
    match guard.get(&project) {
        Some(previous) if previous.last_payload_json == digest => false,
        Some(previous)
            if !force_immediate && previous.last_pushed_at.elapsed() < COLLAPSE_PUSH_INTERVAL =>
        {
            false
        }
        _ => true,
    }
}

fn into_collapse_item(rollup: FleetCollapseHourRollup) -> FleetRollupFeedItem {
    FleetRollupFeedItem {
        hour_start: rollup.hour_start,
        lands: rollup.lands,
        issues_dispatched: rollup.issues_dispatched,
        median_filed_to_landed_secs: rollup.median_filed_to_landed_secs,
        median_dispatched_to_landed_secs: rollup.median_dispatched_to_landed_secs,
        idle_slot_seconds: rollup.idle_slot_seconds,
        tokens_in: rollup.tokens_in,
        tokens_out: rollup.tokens_out,
        tokens_per_landed_change: rollup.tokens_per_landed_change,
        post_land_failures: rollup.post_land_failures,
        reverts: rollup.reverts,
        checks_queued: rollup.checks_queued,
        check_queue_wait_ms_total: rollup.check_queue_wait_ms_total,
        check_queue_wait_ms_median: rollup.check_queue_wait_ms_median,
        check_run_ms_total: rollup.check_run_ms_total,
        retries: rollup.retries,
        review_findings: rollup.review_findings,
    }
}
