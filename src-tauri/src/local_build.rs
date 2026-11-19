//! Velocity on its own landed fixes (SPEC.md F1 "Local builds", #512). When the project
//! that is Velocity's own repository has a new green integration commit, a release of that
//! commit is built in the background, and the title bar offers a restart into it.
//! `velocity-updates` holds the builds and what is offered, `velocity-threads` runs the
//! build, and the restart takes the update's path, so state is kept and threads resume.
//! Nothing here is published: releases stay on CI, which alone holds the updater key.
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_github::velocity_reports::VELOCITY_REPOSITORY;
use velocity_store::duty::LandingProject;
use velocity_threads::local_release::{self, LocalRelease};
use velocity_updates::local_build::{
    FailedBuild, LocalBuild, LocalBuildPhase, LocalBuildState, LocalBuildView, LocalBuilds,
};

use crate::state::ManagedStateStore;

/// The first look waits until startup work and resumed threads settle.
const FIRST_LOOK: Duration = Duration::from_secs(90);
const LOOK_INTERVAL: Duration = Duration::from_secs(60);
/// What a build needs free before it starts: two compiler jobs at about 1.5 GB each
/// (`scripts/release-local.ps1`) and the link.
const BUILD_MEMORY: u64 = 4 * 1024 * 1024 * 1024;

pub struct ManagedLocalBuilds {
    state: Mutex<LocalBuildState>,
    builds: LocalBuilds,
    app_data: PathBuf,
}

#[derive(Clone, serde::Serialize, specta::Type, Event)]
#[tauri_specta(event_name = "local-build://status")]
pub struct LocalBuildStatusChanged {
    pub view: LocalBuildView,
}

fn state(local: &ManagedLocalBuilds) -> MutexGuard<'_, LocalBuildState> {
    local
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn initialize(application: &AppHandle) {
    let Ok(app_data) = application.path().app_data_dir() else {
        return;
    };
    let builds = LocalBuilds::new(&app_data);
    let version = application.package_info().version.to_string();
    let known = LocalBuildState::new(&version, builds.list(), builds.failure());
    application.manage(ManagedLocalBuilds {
        state: Mutex::new(known),
        builds,
        app_data,
    });
    // A development build is not the copy the owner runs: it builds and replaces nothing.
    if cfg!(debug_assertions) || !cfg!(windows) {
        return;
    }
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_LOOK).await;
        loop {
            look(&handle).await;
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    });
}

#[tauri::command]
#[specta::specta]
pub fn get_local_build_status(application: AppHandle) -> LocalBuildView {
    view(&application)
}

/// Restarts Velocity into the kept local build of `commit`. A failure shows in the title
/// bar's status line, never as an error the window keeps showing.
#[tauri::command]
#[specta::specta]
pub async fn restart_into_local_build(application: AppHandle, commit: String) -> LocalBuildView {
    // A refusal changes nothing: the title bar already shows why its buttons are off.
    if let Some(build) = begin_restart(&application, &commit)
        && let Err(message) = restart(&application, &build).await
    {
        change(&application, |state| {
            state.phase = LocalBuildPhase::RestartFailed(message);
        });
    }
    view(&application)
}

/// Builds the project's last green commit when this copy neither runs it nor has it.
async fn look(application: &AppHandle) {
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return;
    };
    let Some(project) = velocity_project(application) else {
        return;
    };
    let server = velocity_rosen::ProjectServer::new(&local.app_data, project.project.id);
    let observed = tauri::async_runtime::spawn_blocking(move || {
        let green = velocity_rosen::integration::last_green(&server);
        (green, velocity_rosen::memory::has_headroom(BUILD_MEMORY))
    })
    .await;
    let Ok((Ok(green), headroom)) = observed else {
        return;
    };
    let wanted = state(&local).wanted(green.as_deref());
    match wanted {
        Some(commit) if headroom => build(application, &local, &project, &commit).await,
        Some(commit) => change(application, |state| {
            state.phase = LocalBuildPhase::WaitingForMemory(commit);
        }),
        None => change(application, |state| {
            if matches!(state.phase, LocalBuildPhase::WaitingForMemory(_)) {
                state.phase = LocalBuildPhase::Idle;
            }
        }),
    }
}

/// The Rosen project whose repository is Velocity's own, if this machine has one.
fn velocity_project(application: &AppHandle) -> Option<LandingProject> {
    let store = application.try_state::<ManagedStateStore>()?;
    let store = store.lock().ok()?;
    let project = store.state().projects.iter().find(|project| {
        project.rosen_in_use()
            && project
                .github_remote
                .as_deref()
                .is_some_and(|remote| remote.eq_ignore_ascii_case(VELOCITY_REPOSITORY))
    })?;
    Some(LandingProject {
        project: project.clone(),
        worktrees: store.worktree_preferences(project),
    })
}

async fn build(
    application: &AppHandle,
    local: &ManagedLocalBuilds,
    project: &LandingProject,
    commit: &str,
) {
    let version = state(local).version_for(commit);
    change(application, |state| {
        state.phase = LocalBuildPhase::Building(commit.to_owned());
    });
    let built = match local.builds.prepare(commit) {
        Ok(out_dir) => {
            let release = LocalRelease {
                commit: commit.to_owned(),
                version: version.clone(),
                out_dir,
            };
            local_release::build(&local.app_data, project, &release).await
        }
        Err(error) => Err(error.to_string()),
    };
    let finished = built.and_then(|()| {
        local
            .builds
            .finish(commit, &version)
            .map_err(|error| error.to_string())
    });
    match finished {
        Ok(_) => change(application, |state| state.built(local.builds.list())),
        Err(reason) => {
            let failure = FailedBuild {
                commit: commit.to_owned(),
                reason,
            };
            // Unsaved, the failure is only forgotten at the next start, which tries again.
            if let Err(error) = local.builds.record_failure(&failure) {
                crate::diagnostics::record("local build", &error.to_string());
            }
            crate::diagnostics::record("local build", &failure.reason);
            change(application, |state| state.build_failed(failure));
        }
    }
}

/// Claims the restart, so a second click does nothing. `None` when the build is no longer
/// offered, or a build or another restart is under way.
fn begin_restart(application: &AppHandle, commit: &str) -> Option<LocalBuild> {
    let local = application.try_state::<ManagedLocalBuilds>()?;
    let build = {
        let mut state = state(&local);
        if state.busy() {
            return None;
        }
        let build = state.offer(commit)?.clone();
        state.phase = LocalBuildPhase::Restarting(commit.to_owned());
        build
    };
    publish(application);
    Some(build)
}

async fn restart(application: &AppHandle, build: &LocalBuild) -> Result<(), String> {
    if cfg!(debug_assertions) {
        return Err(
            "Development builds don't restart into local builds. Use a release build.".into(),
        );
    }
    let local = application
        .try_state::<ManagedLocalBuilds>()
        .ok_or("Local builds haven't started yet")?;
    let (builds, read) = (local.builds.clone(), build.clone());
    let bytes = tauri::async_runtime::spawn_blocking(move || builds.read_executable(&read))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let folder = local.builds.folder_of(&build.commit);
    crate::updates::hand_over(application, "Velocity was not restarted", || {
        // A copy installed where Velocity may not write can't be swapped in place.
        crate::updates::restart_portable(application, &bytes).map_err(|error| {
            format!(
                "{error}. The setup for this build is in {}.",
                folder.display()
            )
        })
    })
    .await
}

/// Applies `update` and tells the window when what the title bar shows changed.
fn change(application: &AppHandle, update: impl FnOnce(&mut LocalBuildState)) {
    let Some(local) = application.try_state::<ManagedLocalBuilds>() else {
        return;
    };
    let changed = {
        let mut state = state(&local);
        let before = state.view();
        update(&mut state);
        before != state.view()
    };
    if changed {
        publish(application);
    }
}

fn view(application: &AppHandle) -> LocalBuildView {
    application
        .try_state::<ManagedLocalBuilds>()
        .map(|local| state(&local).view())
        .unwrap_or_default()
}

fn publish(application: &AppHandle) {
    let view = view(application);
    if let Err(error) = (LocalBuildStatusChanged { view }).emit(application) {
        eprintln!("Could not publish the local build status: {error}");
    }
}
