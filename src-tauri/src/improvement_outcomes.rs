//! Background post-land metric evaluator for self-improvement issues (#621).
//!
//! Periodically (and whenever a Rosen change lands) checks every landed issue with a
//! `Target metric:` line whose post-land window (`max(24 hours, 20 lands)`) has closed,
//! persists the measured outcome into `fleet_ledger.improvement_outcomes`, posts an
//! idempotent outcome comment on the issue, and files a follow-up `self-improvement`
//! issue when the metric clearly regressed (>= 25% worse with >= 10 samples per side).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use tokio::sync::Notify;
use velocity_core::fleet_ledger::FleetLedger;
use velocity_core::improvement_outcomes::{
    ImprovementOutcomeRecord, ImprovementVerdict, build_regression_followup_draft,
    evaluate_pending_outcomes, format_outcome_comment, outcome_comment_marker,
};
use velocity_github::{IssueClient, IssueHost};

use crate::state::ManagedStateStore;
use crate::threads::lock;

const LOG_SOURCE: &str = "improvement outcomes";
const STARTUP_DELAY: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
const RETRY_COOLDOWN: Duration = Duration::from_secs(5 * 60);

static WAKE_SIGNAL: OnceLock<Notify> = OnceLock::new();
static RETRY_BACKOFF: OnceLock<Mutex<BTreeMap<String, Instant>>> = OnceLock::new();

#[derive(Clone)]
struct ProjectOutcomeContext {
    id: u32,
    name: String,
    directory: PathBuf,
    host: IssueHost,
    synced_numbers: BTreeMap<u32, u32>,
    ledger: FleetLedger,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Starts the background self-improvement outcome evaluator for the life of the app (#621).
pub fn initialize(application: &AppHandle) {
    let notify = WAKE_SIGNAL.get_or_init(Notify::new);
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        loop {
            if !crate::rosen::paused(&handle) {
                evaluate_all_projects(&handle).await;
            }
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                _ = notify.notified() => {}
            }
        }
    });
}

/// Wakes the evaluator promptly after a Rosen land is recorded in `fleet_ledger`.
pub(crate) fn wake() {
    if let Some(notify) = WAKE_SIGNAL.get() {
        notify.notify_one();
    }
}

async fn evaluate_all_projects(application: &AppHandle) {
    let contexts = snapshot_projects(application);
    let now = unix_now();
    for context in contexts {
        evaluate_project(application, context, now).await;
    }
}

fn snapshot_projects(application: &AppHandle) -> Vec<ProjectOutcomeContext> {
    let Ok(store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return Vec::new();
    };
    store
        .state()
        .projects
        .iter()
        .filter(|project| project.rosen_in_use())
        .map(|project| ProjectOutcomeContext {
            id: project.id,
            name: project.name.clone(),
            directory: PathBuf::from(&project.repository_path),
            host: project.effective_issue_host(),
            synced_numbers: project
                .github_issue_sync
                .as_ref()
                .map(|sync| sync.synced_numbers.clone())
                .unwrap_or_default(),
            ledger: project.fleet_ledger.clone(),
        })
        .collect()
}

async fn evaluate_project(application: &AppHandle, mut context: ProjectOutcomeContext, now: u64) {
    let newly_measured = evaluate_pending_outcomes(&context.ledger, now);
    if !newly_measured.is_empty()
        && let Ok(mut store) = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
    {
        for outcome in newly_measured {
            let _ = store.record_fleet_improvement_outcome(context.id, outcome);
        }
        if let Some(updated) = store.fleet_ledger(context.id) {
            context.ledger = updated;
        }
    }

    let actionable: Vec<ImprovementOutcomeRecord> = context
        .ledger
        .improvement_outcomes
        .iter()
        .filter(|outcome| needs_host_delivery(outcome))
        .cloned()
        .collect();
    for outcome in actionable {
        deliver_outcome_effects(application, &context, outcome, now).await;
    }
}

fn needs_host_delivery(outcome: &ImprovementOutcomeRecord) -> bool {
    if outcome.verdict == ImprovementVerdict::UnsupportedMetric {
        return false;
    }
    outcome.commented_at.is_none()
        || (outcome.verdict == ImprovementVerdict::Regressed && outcome.regression_issue.is_none())
}

async fn deliver_outcome_effects(
    application: &AppHandle,
    context: &ProjectOutcomeContext,
    mut outcome: ImprovementOutcomeRecord,
    now: u64,
) {
    let retry_key = format!(
        "{}:{}#{}:{}",
        context.id,
        outcome.repository.to_ascii_lowercase(),
        outcome.issue,
        outcome.landed_at
    );
    if in_retry_cooldown(&retry_key) {
        return;
    }
    let mut had_error = false;
    if outcome.verdict == ImprovementVerdict::Regressed && outcome.regression_issue.is_none() {
        had_error |= !deliver_regression_followup(application, context, &mut outcome).await;
    }
    if outcome.commented_at.is_none() {
        had_error |= !deliver_outcome_comment(application, context, &outcome, now).await;
    }
    if had_error {
        record_retry_cooldown(retry_key);
    } else {
        clear_retry_cooldown(&retry_key);
    }
}

async fn deliver_regression_followup(
    application: &AppHandle,
    context: &ProjectOutcomeContext,
    outcome: &mut ImprovementOutcomeRecord,
) -> bool {
    match file_regression_issue(context, outcome).await {
        Ok(Some((number, url))) => {
            outcome.regression_issue = Some(number);
            outcome.regression_issue_url = Some(url.clone());
            if let Ok(mut store) = lock(
                application.state::<ManagedStateStore>().inner(),
                "persisted state",
            ) {
                let _ = store.record_fleet_improvement_regression_issue(
                    context.id,
                    &outcome.repository,
                    outcome.issue,
                    outcome.landed_at,
                    number,
                    Some(&url),
                );
            }
            crate::diagnostics::record(
                LOG_SOURCE,
                &format!(
                    "{}: filed self-improvement regression follow-up #{number} for #{}",
                    context.name, outcome.issue
                ),
            );
            true
        }
        Ok(None) => true,
        Err(error) => {
            crate::diagnostics::record(
                LOG_SOURCE,
                &format!(
                    "{}: could not file regression follow-up for #{}: {error}",
                    context.name, outcome.issue
                ),
            );
            false
        }
    }
}

async fn deliver_outcome_comment(
    application: &AppHandle,
    context: &ProjectOutcomeContext,
    outcome: &ImprovementOutcomeRecord,
    now: u64,
) -> bool {
    match post_outcome_comment(context, outcome).await {
        Ok(()) => {
            if let Ok(mut store) = lock(
                application.state::<ManagedStateStore>().inner(),
                "persisted state",
            ) {
                let _ = store.mark_fleet_improvement_commented(
                    context.id,
                    &outcome.repository,
                    outcome.issue,
                    outcome.landed_at,
                    now,
                );
            }
            crate::diagnostics::record(
                LOG_SOURCE,
                &format!(
                    "{}: posted self-improvement outcome on #{} ({})",
                    context.name,
                    outcome.issue,
                    outcome.verdict.label()
                ),
            );
            true
        }
        Err(error) => {
            crate::diagnostics::record(
                LOG_SOURCE,
                &format!(
                    "{}: could not comment outcome on #{}: {error}",
                    context.name, outcome.issue
                ),
            );
            false
        }
    }
}

async fn post_outcome_comment(
    context: &ProjectOutcomeContext,
    outcome: &ImprovementOutcomeRecord,
) -> Result<(), String> {
    let directory = context.directory.clone();
    let host = context.host;
    let repository = outcome.repository.clone();
    let target_number = if host == IssueHost::Collapse {
        context
            .synced_numbers
            .get(&outcome.issue)
            .copied()
            .unwrap_or(outcome.issue)
    } else {
        outcome.issue
    };
    let marker = outcome_comment_marker(outcome);
    let body = format_outcome_comment(outcome);
    tauri::async_runtime::spawn_blocking(move || {
        let client = IssueClient::for_host(&directory, host, &repository)
            .map_err(|error| error.to_string())?;
        client
            .post_improvement_outcome_comment(target_number, &marker, &body)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn file_regression_issue(
    context: &ProjectOutcomeContext,
    outcome: &ImprovementOutcomeRecord,
) -> Result<Option<(u32, String)>, String> {
    let Some(draft) = build_regression_followup_draft(outcome) else {
        return Ok(None);
    };
    let directory = context.directory.clone();
    let host = context.host;
    let repository = outcome.repository.clone();
    let filed = tauri::async_runtime::spawn_blocking(move || {
        let client = IssueClient::for_host(&directory, host, &repository)
            .map_err(|error| error.to_string())?;
        client
            .file_improvement_regression_issue(&draft)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    Ok(Some((filed.number, filed.url)))
}

fn in_retry_cooldown(key: &str) -> bool {
    let map = RETRY_BACKOFF.get_or_init(|| Mutex::new(BTreeMap::new()));
    map.lock()
        .ok()
        .and_then(|guard| guard.get(key).copied())
        .is_some_and(|at| at.elapsed() < RETRY_COOLDOWN)
}

fn record_retry_cooldown(key: String) {
    let map = RETRY_BACKOFF.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(mut guard) = map.lock() {
        guard.insert(key, Instant::now());
    }
}

fn clear_retry_cooldown(key: &str) {
    let map = RETRY_BACKOFF.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Ok(mut guard) = map.lock() {
        guard.remove(key);
    }
}
