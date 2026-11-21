//! Keeping Collapse's pinned `velocity-graph` indexer current (SPEC.md F19 "Standalone
//! indexer", #577). Mirrors `rosen_pin.rs` (#513): in a release build on a machine with a
//! Velocity or Collapse Rosen project whose signed-in account may push to Collapse's
//! repository, this edge checks every ten minutes whether a published Velocity release
//! changes `velocity-graph`'s parser or schema version and keeps exactly one open pin-bump
//! issue on Collapse. No thread, agent session or window is involved.
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_github::graph_indexer_pin::{
    COLLAPSE_REPOSITORIES, GraphIndexerPinWatch, GraphPinCache, IssueLook, PinIssue,
};
use velocity_github::velocity_reports::VELOCITY_REPOSITORY;

use crate::state::ManagedStateStore;

/// Wait until startup work and resumed threads settle, offset slightly from `rosen_pin`.
const FIRST_LOOK: Duration = Duration::from_secs(2 * 60 + 15);
const LOOK_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// What the looks remember between them. Nothing is saved: a restart checks GitHub again.
#[derive(Default)]
struct Watch {
    /// Resolved Collapse GitHub `owner/name` repository that the signed-in account maintains.
    collapse_repository: Option<String>,
    /// Cached immutable release metadata (parser/schema versions and SHA-256 assets).
    cache: GraphPinCache,
    /// The failure last written to the log, so a failure that repeats is written once.
    last_failure: Option<String>,
}

pub fn initialize(application: &AppHandle) {
    // A development build is not the copy the owner runs: it files nothing.
    if cfg!(debug_assertions) {
        return;
    }
    let application = application.clone();
    std::thread::spawn(move || {
        let mut watch = Watch::default();
        std::thread::sleep(FIRST_LOOK);
        loop {
            let candidates = collapse_candidates(&application);
            if !candidates.is_empty() {
                look(&mut watch, &candidates);
            }
            std::thread::sleep(LOOK_INTERVAL);
        }
    });
}

/// Ordered Collapse GitHub repository candidates when this machine has a Rosen project for
/// Velocity or Collapse. Empty when neither project is present.
fn collapse_candidates(application: &AppHandle) -> Vec<String> {
    let Some(store) = application.try_state::<ManagedStateStore>() else {
        return Vec::new();
    };
    let Ok(store) = store.lock() else {
        return Vec::new();
    };
    let mut configured = Vec::new();
    let mut relevant = false;
    for project in &store.state().projects {
        if !project.rosen_in_use() {
            continue;
        }
        let remote = project.github_remote.as_deref();
        if remote.is_some_and(|repo| repo.eq_ignore_ascii_case(VELOCITY_REPOSITORY)) {
            relevant = true;
        }
        let collapse_remote = remote.filter(|repo| {
            is_collapse_repository(repo)
                || project
                    .collapse_repository
                    .as_deref()
                    .is_some_and(is_collapse_repository)
        });
        if let Some(repo) = collapse_remote {
            relevant = true;
            push_unique(&mut configured, repo);
        }
    }
    if !relevant {
        return Vec::new();
    }
    for default_repo in COLLAPSE_REPOSITORIES {
        push_unique(&mut configured, default_repo);
    }
    configured
}

fn is_collapse_repository(repository: &str) -> bool {
    COLLAPSE_REPOSITORIES
        .iter()
        .any(|known| repository.eq_ignore_ascii_case(known))
}

fn push_unique(list: &mut Vec<String>, candidate: &str) {
    if !list
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(candidate))
    {
        list.push(candidate.to_owned());
    }
}

fn look(watch: &mut Watch, candidates: &[String]) {
    let (done, failure) = match observe(watch, candidates) {
        Ok(Some((repository, look))) => (issue_notes(&repository, &look), None),
        Ok(None) => (Vec::new(), None),
        Err(error) => (
            Vec::new(),
            Some(format!("The Collapse indexer pin was not checked: {error}")),
        ),
    };
    for note in &done {
        crate::diagnostics::record("graph indexer pin", note);
    }
    if let Some(failure) = failure.as_deref().filter(|_| failure != watch.last_failure) {
        crate::diagnostics::record("graph indexer pin", failure);
    }
    watch.last_failure = failure;
}

/// One look, or `None` when the signed-in account does not maintain Collapse's repository.
fn observe(
    watch: &mut Watch,
    candidates: &[String],
) -> Result<Option<(String, IssueLook)>, velocity_github::GitHubCommandError> {
    let client = match watch.collapse_repository.as_deref() {
        Some(collapse) => GraphIndexerPinWatch::new(VELOCITY_REPOSITORY, collapse)?,
        None => {
            let Some(client) = resolve_maintainer_client(candidates)? else {
                return Ok(None);
            };
            watch.collapse_repository = Some(client.collapse_repository().to_owned());
            client
        }
    };
    let repository = client.collapse_repository().to_owned();
    let result = client.look(&mut watch.cache)?;
    Ok(Some((repository, result)))
}

fn resolve_maintainer_client(
    candidates: &[String],
) -> Result<Option<GraphIndexerPinWatch>, velocity_github::GitHubCommandError> {
    for candidate in candidates {
        let client = GraphIndexerPinWatch::new(VELOCITY_REPOSITORY, candidate)?;
        match client.can_maintain()? {
            Some(true) => return Ok(Some(client)),
            Some(false) => return Ok(None),
            None => continue,
        }
    }
    Ok(None)
}

fn issue_notes(repository: &str, look: &IssueLook) -> Vec<String> {
    let mut notes = Vec::new();
    match look.issue {
        PinIssue::Opened(number) => {
            notes.push(format!(
                "Opened {repository}#{number} to bump velocity-graph."
            ));
        }
        PinIssue::Rewritten(number) => {
            notes.push(format!(
                "Rewrote {repository}#{number} for a newer velocity-graph release."
            ));
        }
        PinIssue::Current | PinIssue::Kept(_) => {}
    }
    if !look.closed.is_empty() {
        let closed: Vec<String> = look
            .closed
            .iter()
            .map(|number| format!("{repository}#{number}"))
            .collect();
        notes.push(format!(
            "Closed superseded velocity-graph pin-bump issues {}.",
            closed.join(", ")
        ));
    }
    notes
}
