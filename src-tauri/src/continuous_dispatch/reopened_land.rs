//! Rosen land-history guard for reopened issues (#615).
//!
//! #602 makes reopened issues dispatchable again when a person reopens a regression.
//! Before continuous dispatch starts an agent, this module checks Rosen's non-reverted
//! base land history for a landed commit named for the issue (`(#N)`) after its last
//! reopen by a person. Matching issues are persisted in `reopened_landed_holds`, skipped
//! by dispatch, and surfaced immediately in project health.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use velocity_core::project_health::short_commit;
use velocity_core::reopened_land::{
    LandHistoryIndex, OpenIssueReopenCheck, match_reopened_already_landed,
    reopened_already_landed_reason,
};
use velocity_github::{GitHubIssue, IssueHold, IssueState};
use velocity_rosen::post_land::{current_base_tip, recent_land_history};
use velocity_store::continuous_dispatch::ReopenedLandedHold;
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::rosen_work::IssueVersion;
use velocity_store::{ProjectItem, ThreadState};

use super::Backlog;
use crate::state::ManagedStateStore;
use crate::threads::lock;

const LAND_HISTORY_LIMIT: u32 = 2000;

#[derive(Clone)]
struct CachedHistory {
    base_tip: Option<String>,
    index: LandHistoryIndex,
}

#[derive(Default)]
pub(crate) struct ManagedLandHistory(Mutex<BTreeMap<u32, CachedHistory>>);

pub(crate) fn initialize(application: &AppHandle) {
    application.manage(ManagedLandHistory::default());
}

/// Formats an issue-panel hold for a reopened issue whose fix already landed (#615).
pub(crate) fn issue_hold(hold: &ReopenedLandedHold) -> IssueHold {
    let short = short_commit(&hold.commit);
    let reason = if hold.reopened_by_import {
        format!(
            "Reopened by an import, but land history already contains {short} ({}). \
             Close on Collapse or edit the issue to dispatch it again.",
            hold.land_summary
        )
    } else {
        format!(
            "Land history already contains {short} ({}) after this issue's last reopen by a \
             person. Close the issue or reopen/edit it to dispatch it again.",
            hold.land_summary
        )
    };
    IssueHold::new(
        reopened_already_landed_reason(&hold.commit),
        reason,
        Vec::new(),
    )
}

/// Refreshes `reopened_landed_holds` for `project` against Rosen's land history (#615).
pub(super) fn refresh(application: &AppHandle, project: &ProjectItem, backlog: &Backlog) {
    if !project.rosen_in_use() || backlog.error.is_some() || backlog.repository.is_empty() {
        return;
    }
    let Some(index) = load_land_history(application, project.id) else {
        return;
    };
    let now_ms = unix_millis();
    let mut next_holds = BTreeMap::new();
    for issue in &backlog.issues {
        if let Some(hold) = evaluate_issue(project, &backlog.repository, issue, &index, now_ms) {
            next_holds.insert(issue.number, hold);
        }
    }
    let changed = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|mut store| {
        store
            .save_reopened_landed_holds(project.id, next_holds.clone())
            .ok()
    })
    .unwrap_or(false);
    if changed {
        log_new_holds(project, &next_holds);
        crate::project_health::sync_reopened_landed_holds(application, project.id);
        crate::issue_panel::refresh_links(application);
    }
}

/// Re-evaluates one freshly read issue right before dispatch so a newly reopened issue
/// cannot slip past a stale backlog snapshot (#615).
pub(crate) fn check_issue(
    application: &AppHandle,
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
) -> Option<ReopenedLandedHold> {
    if !project.rosen_in_use() || repository.is_empty() {
        return None;
    }
    let index = load_land_history(application, project.id)?;
    let now_ms = unix_millis();
    let hold = evaluate_issue(project, repository, issue, &index, now_ms);
    let changed = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|mut store| match &hold {
        Some(record) => store
            .upsert_reopened_landed_hold(project.id, record.clone())
            .ok(),
        None => store
            .remove_reopened_landed_hold(project.id, issue.number)
            .ok(),
    })
    .unwrap_or(false);
    if changed {
        if let Some(record) = &hold {
            record_hold_diagnostic(&project.name, record);
        }
        crate::project_health::sync_reopened_landed_holds(application, project.id);
        crate::issue_panel::refresh_links(application);
    }
    hold
}

fn evaluate_issue(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    index: &LandHistoryIndex,
    now_ms: u64,
) -> Option<ReopenedLandedHold> {
    if issue.state != IssueState::Open {
        return None;
    }
    let identity = ThreadIssue::new(repository.into(), issue.number)
        .with_identity(issue.title.clone(), issue.host);
    let version = IssueVersion::from_github(issue);
    if issue_thread(project, &identity)
        .is_some_and(|thread| matches!(thread.state, ThreadState::Working | ThreadState::Review))
    {
        return None;
    }
    if is_normal_pending_close(project, &identity, issue, version) {
        return None;
    }
    if let Some(existing) = project.reopened_landed_hold(&identity, version)
        && index
            .latest_land(issue.number)
            .is_some_and(|land| land.commit.eq_ignore_ascii_case(&existing.commit))
    {
        let mut kept = existing.clone();
        kept.reopened_by_import |= issue.reopened_by_import;
        return Some(kept);
    }
    let matched = match_reopened_already_landed(
        OpenIssueReopenCheck {
            number: issue.number,
            updated_at: &issue.updated_at,
            person_reopened_at: issue.reopened_at.as_deref(),
            reopened_by_import: issue.reopened_by_import,
            reopened_by_non_person: issue.reopened_by_non_person,
        },
        index,
    )?;
    Some(ReopenedLandedHold {
        issue: identity,
        commit: matched.commit,
        landed_at: matched.landed_at,
        land_summary: matched.summary,
        updated_at: issue.updated_at.clone(),
        reopened_by_import: matched.reopened_by_import,
        detected_at_ms: now_ms,
    })
}

/// Normal post-land issues waiting for integration or the 30-minute review window to close
/// are not reopened issues and stay under `landed_awaiting_integration` / `rosen_issue` (#615).
fn is_normal_pending_close(
    project: &ProjectItem,
    identity: &ThreadIssue,
    issue: &GitHubIssue,
    version: IssueVersion<'_>,
) -> bool {
    if issue.reopened_by_import || issue.reopened_by_non_person || issue.reopened_at.is_some() {
        return false;
    }
    project
        .landed_awaiting_integration_for_version(identity, version)
        .is_some()
        || project.has_pending_rosen_close(identity)
}

fn load_land_history(application: &AppHandle, project: u32) -> Option<LandHistoryIndex> {
    let server = crate::rosen::observation_server(application, project).ok()?;
    let tip = tauri::async_runtime::block_on(current_base_tip(&server)).ok()?;
    if let Some(managed) = application.try_state::<ManagedLandHistory>()
        && let Ok(cache) = lock(&managed.0, "land history")
        && let Some(existing) = cache.get(&project)
        && existing.base_tip == tip
    {
        return Some(existing.index.clone());
    }
    let (loaded_tip, entries, reverted) =
        tauri::async_runtime::block_on(recent_land_history(&server, LAND_HISTORY_LIMIT)).ok()?;
    let index = LandHistoryIndex::from_entries(&entries, &reverted);
    if let Some(managed) = application.try_state::<ManagedLandHistory>()
        && let Ok(mut cache) = lock(&managed.0, "land history")
    {
        cache.insert(
            project,
            CachedHistory {
                base_tip: loaded_tip.or(tip),
                index: index.clone(),
            },
        );
    }
    Some(index)
}

fn log_new_holds(project: &ProjectItem, next_holds: &BTreeMap<u32, ReopenedLandedHold>) {
    for (number, hold) in next_holds {
        if !project
            .continuous_dispatch
            .reopened_landed_holds
            .contains_key(number)
        {
            record_hold_diagnostic(&project.name, hold);
        }
    }
}

fn record_hold_diagnostic(project_name: &str, hold: &ReopenedLandedHold) {
    let origin = if hold.reopened_by_import {
        "reopened by import, "
    } else {
        ""
    };
    crate::diagnostics::record(
        "project health",
        &format!(
            "{project_name}: held #{} ({origin}{})",
            hold.issue.number,
            reopened_already_landed_reason(&hold.commit)
        ),
    );
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
