//! Keeping Velocity's Rosen pin current (SPEC.md F5 "Rosen · Pin bumps", #513). Where a
//! Rosen project's GitHub repository is Velocity's own, this edge looks every ten minutes
//! for a green commit on Rosen's `main` ahead of the pin and keeps exactly one open, current
//! "Bump the Rosen pin to `<commit>`" issue, which continuous dispatch then takes like any
//! other. It also publishes the Rosen release a landed bump names. No thread, agent session
//! or window is involved; `velocity-core` decides and `velocity-github` talks to GitHub.
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_github::rosen_pin::{IssueLook, PinIssue, PinLook, ReleaseLook, RosenPinWatch};
use velocity_github::velocity_reports::VELOCITY_REPOSITORY;

use crate::state::ManagedStateStore;

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
                look(&mut watch);
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

fn look(watch: &mut Watch) {
    let (done, failures) = match observe(watch) {
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
fn observe(watch: &mut Watch) -> Result<Option<PinLook>, velocity_github::GitHubCommandError> {
    let client = RosenPinWatch::new(VELOCITY_REPOSITORY)?;
    if !watch.maintainer {
        watch.maintainer = client.can_maintain()?;
    }
    if !watch.maintainer {
        return Ok(None);
    }
    client.look(watch.known_release.as_deref()).map(Some)
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
