//! Keeping Velocity's Rosen pin current (SPEC.md F5 "Rosen · Pin bumps", #513). Where a
//! Rosen project's GitHub repository is Velocity's own, this edge looks every ten minutes
//! for a green commit on Rosen's `main` ahead of the pin and keeps exactly one open, current
//! "Bump the Rosen pin to `<commit>`" issue, which continuous dispatch then takes like any
//! other. It also publishes the Rosen release a landed bump names. No thread, agent session
//! or window is involved; `velocity-core` decides and `velocity-github` talks to GitHub.
use std::collections::BTreeMap;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_core::rosen_pin::{
    PostLandSnapshot, PostLandState, RosenCommit, RosenPin, merge_post_land_states,
};
use velocity_github::rosen_pin::{IssueLook, PinIssue, PinLook, ReleaseLook, RosenPinWatch};
use velocity_github::velocity_reports::VELOCITY_REPOSITORY;

use crate::state::ManagedStateStore;

/// How many newest commits ahead of the pin are checked for green status.
const GREEN_SCAN: usize = 8;

/// The first look waits until startup work and resumed threads settle.
const FIRST_LOOK: Duration = Duration::from_secs(2 * 60);
/// Rosen lands a few commits an hour and its CI takes longer than this, so a green commit
/// has its issue within minutes at about ten requests a look.
const LOOK_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// What the looks remember between them. Nothing is saved: a restart only asks GitHub again.
#[derive(Default)]
struct Watch {
    /// The signed-in account may push to Velocity's repository.
    maintainer: bool,
    /// The pinned Rosen release already found on Rosen.
    known_release: Option<String>,
    /// The failure last written to the log, so a failure that repeats is written once.
    last_failure: Option<String>,
}

pub fn initialize(application: &AppHandle) {
    // A development build is not the copy the owner runs: it files and publishes nothing.
    if cfg!(debug_assertions) {
        return;
    }
    let application = application.clone();
    std::thread::spawn(move || {
        let mut watch = Watch::default();
        std::thread::sleep(FIRST_LOOK);
        loop {
            if has_velocity_project(&application) {
                look(&application, &mut watch);
            }
            std::thread::sleep(LOOK_INTERVAL);
        }
    });
}

/// Whether this machine has the Rosen project whose repository is Velocity's own, as the
/// owner's does (the same test as local builds, F1).
fn has_velocity_project(application: &AppHandle) -> bool {
    let Some(store) = application.try_state::<ManagedStateStore>() else {
        return false;
    };
    let Ok(store) = store.lock() else {
        return false;
    };
    store.state().projects.iter().any(|project| {
        project.rosen_in_use()
            && project
                .github_remote
                .as_deref()
                .is_some_and(|remote| remote.eq_ignore_ascii_case(VELOCITY_REPOSITORY))
    })
}

fn look(application: &AppHandle, watch: &mut Watch) {
    let (done, failures) = match observe(application, watch) {
        Ok(Some(PinLook { issue, release })) => {
            let mut done = Vec::new();
            let mut failures = Vec::new();
            match issue {
                Ok(issue) => done.extend(issue_notes(&issue)),
                Err(error) => failures.push(format!("The pin-bump issue was not updated: {error}")),
            }
            match release {
                Ok(release) => {
                    done.extend(release_note(&release));
                    watch.known_release = release.release;
                }
                Err(error) => {
                    failures.push(format!(
                        "The pinned Rosen release was not published: {error}"
                    ));
                }
            }
            (done, failures)
        }
        Ok(None) => (Vec::new(), Vec::new()),
        Err(error) => (
            Vec::new(),
            vec![format!("The Rosen pin was not read: {error}")],
        ),
    };
    for note in &done {
        crate::diagnostics::record("rosen pin", note);
    }
    let failure = (!failures.is_empty()).then(|| failures.join(" "));
    if let Some(failure) = failure.as_deref().filter(|_| failure != watch.last_failure) {
        crate::diagnostics::record("rosen pin", failure);
    }
    watch.last_failure = failure;
}

/// One look, or `None` when the signed-in account does not maintain Velocity's repository.
fn observe(
    application: &AppHandle,
    watch: &mut Watch,
) -> Result<Option<PinLook>, velocity_github::GitHubCommandError> {
    let client = RosenPinWatch::new(VELOCITY_REPOSITORY)?;
    if !watch.maintainer {
        watch.maintainer = client.can_maintain()?;
    }
    if !watch.maintainer {
        return Ok(None);
    }
    client
        .look_with_post_land(watch.known_release.as_deref(), |pin, ahead| {
            read_post_land(application, pin, ahead)
        })
        .map(Some)
}

/// Reads Rosen's post-land verdicts from this machine's local Rosen server for `pin.repository`
/// when present, falling back to Collapse's post-land read when needed (#580).
fn read_post_land(
    application: &AppHandle,
    pin: &RosenPin,
    ahead: &[RosenCommit],
) -> BTreeMap<String, PostLandState> {
    let mut states = BTreeMap::new();
    let local = local_rosen_project(application, &pin.repository);
    let candidate_commits: Vec<String> = ahead
        .iter()
        .take(GREEN_SCAN)
        .map(|commit| commit.commit.clone())
        .collect();
    if let Some((project_id, _)) = &local {
        match crate::rosen::observation_server(application, *project_id).and_then(|server| {
            velocity_rosen::integration::post_land_states(&server, &candidate_commits)
                .map_err(|error| error.to_string())
        }) {
            Ok(local_states) => states = local_states,
            Err(error) => crate::diagnostics::record(
                "rosen pin",
                &format!("Could not read local Rosen post-land state: {error}"),
            ),
        }
    }
    let unresolved = candidate_commits
        .iter()
        .any(|commit| !states.contains_key(&commit.to_ascii_lowercase()));
    if local.is_none() || unresolved {
        let collapse_repo = local.and_then(|(_, collapse)| collapse);
        if let Some(collapse_states) = read_collapse_post_land(pin, collapse_repo.as_deref(), ahead)
        {
            merge_post_land_states(&mut states, collapse_states);
        }
    }
    states
}

/// Finds a local Rosen project on this machine corresponding to `repository` (such as
/// `mESmaC/rosen`), returning `(project_id, collapse_repository)`.
fn local_rosen_project(application: &AppHandle, repository: &str) -> Option<(u32, Option<String>)> {
    let store = application.try_state::<ManagedStateStore>()?;
    let store = store.lock().ok()?;
    store.state().projects.iter().find_map(|project| {
        if !project.rosen_in_use() {
            return None;
        }
        let matches = project
            .github_remote
            .as_deref()
            .is_some_and(|remote| remote.eq_ignore_ascii_case(repository))
            || project
                .collapse_mirror_health
                .as_ref()
                .and_then(|health| health.downstream_github.as_deref())
                .is_some_and(|remote| remote.eq_ignore_ascii_case(repository))
            || project
                .collapse_repository
                .as_deref()
                .is_some_and(|remote| remote.eq_ignore_ascii_case(repository));
        matches.then(|| (project.id, project.collapse_repository.clone()))
    })
}

/// Reads Rosen's post-land checkpoint from Collapse when Collapse is connected (#580).
/// Failures are recorded and return `None` so GitHub CI still works when Collapse is away.
fn read_collapse_post_land(
    pin: &RosenPin,
    local_collapse_repo: Option<&str>,
    ahead: &[RosenCommit],
) -> Option<BTreeMap<String, PostLandState>> {
    let credentials = match velocity_collapse::load_credentials() {
        Ok(Some(credentials)) => credentials,
        Ok(None) => return None,
        Err(error) => {
            crate::diagnostics::record(
                "rosen pin",
                &format!("Could not load Collapse credentials for post-land check: {error}"),
            );
            return None;
        }
    };
    let target = local_collapse_repo.unwrap_or(&pin.repository);
    let post_land = match tauri::async_runtime::block_on(fetch_collapse_post_land(
        &credentials,
        target,
        &pin.repository,
        local_collapse_repo.is_some(),
    )) {
        Ok(Some(post_land)) => post_land,
        Ok(None) => return None,
        Err(error) => {
            crate::diagnostics::record(
                "rosen pin",
                &format!("Could not read {target} post-land state from Collapse: {error}"),
            );
            return None;
        }
    };
    let snapshot = PostLandSnapshot {
        green_commit: post_land.green_commit,
        green_at: post_land.green_at,
        green_events: post_land.green_events,
        failed_commits: post_land.failed_commits,
    };
    Some(snapshot.states_for(ahead))
}

async fn fetch_collapse_post_land(
    credentials: &velocity_collapse::CollapseCredentials,
    primary_repo: &str,
    github_repo: &str,
    has_local_collapse_repo: bool,
) -> Result<Option<velocity_collapse::RepositoryPostLand>, velocity_collapse::CollapseError> {
    if let Some((owner, name)) = primary_repo.split_once('/')
        && let Some(state) =
            velocity_collapse::repository_post_land(credentials, owner, name).await?
    {
        return Ok(Some(state));
    }
    if has_local_collapse_repo {
        return Ok(None);
    }
    // If Rosen's Collapse mirror lives under another namespace, locate it by its GitHub downstream.
    let rows = velocity_collapse::repository_activity(credentials).await?;
    let matched = rows.into_iter().find(|row| {
        row.downstream_remote()
            .as_deref()
            .and_then(velocity_core::collapse::github_repository_from_remote)
            .is_some_and(|downstream| downstream.eq_ignore_ascii_case(github_repo))
    });
    let Some(row) = matched else {
        return Ok(None);
    };
    velocity_collapse::repository_post_land(credentials, &row.owner, &row.name).await
}

fn issue_notes(look: &IssueLook) -> Vec<String> {
    let mut notes = Vec::new();
    match look.issue {
        PinIssue::Opened(number) => notes.push(format!("Opened pin-bump issue #{number}.")),
        PinIssue::Rewritten(number) => notes.push(format!("Rewrote pin-bump issue #{number}.")),
        PinIssue::Current | PinIssue::NoGreenCommit | PinIssue::Kept(_) => {}
    }
    if !look.closed.is_empty() {
        let closed: Vec<String> = look
            .closed
            .iter()
            .map(|number| format!("#{number}"))
            .collect();
        notes.push(format!("Closed pin-bump issues {}.", closed.join(", ")));
    }
    notes
}

fn release_note(look: &ReleaseLook) -> Option<String> {
    let release = look.release.as_deref().filter(|_| look.published)?;
    Some(format!(
        "Published Rosen release {release} for the pinned commit."
    ))
}
