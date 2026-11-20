//! Adapt Rosen's mirror observation to saved project state, the window and AIRC, and
//! catch projects up from their mirror (#501).

use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_core::rosen_mirror::MirrorDivergence;
use velocity_rosen::{CatchUp, ProjectServer};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// Refresh on startup, mirror events and the periodic retry, one observation at a time.
pub(crate) async fn refresh(application: &AppHandle, project: u32) -> Result<(), String> {
    let running = application.state::<crate::rosen::ManagedRosen>();
    let _one_at_a_time = running.follow_ups.lock().await;
    let server = crate::rosen::server(application, project).await?;
    let client = server.connect().await.map_err(|error| error.to_string())?;
    let head = velocity_rosen::EventHead::read(&client)
        .await
        .map_err(|error| error.to_string())?;
    let observation = velocity_rosen::mirror_status(&server)
        .await
        .map_err(|error| error.to_string())?;
    publish(application, project, head, observation)?;
    let remote = velocity_rosen::mirror_remote(&server)
        .await
        .map_err(|error| error.to_string())?;
    // A configuration that could not be read keeps the saved answer, so a linked project
    // is not offered Send Collapse its live state again (#517).
    let live_state = velocity_rosen::collapse_link::collapse_root(&server)
        .await
        .ok()
        .map(|root| root.is_some());
    save_collapse_repository(
        application,
        project,
        remote
            .as_deref()
            .and_then(velocity_core::collapse::repository_from_remote),
        live_state,
    )?;
    let handle = application.clone();
    tauri::async_runtime::spawn_blocking(move || deliver_notices(&handle, project))
        .await
        .map_err(|error| error.to_string())?
}

fn deliver_notices(application: &AppHandle, project: u32) -> Result<(), String> {
    let notices = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project)
    .map(|item| item.rosen_mirror_notices.clone())
    .unwrap_or_default();
    if notices.is_empty() {
        return Ok(());
    }
    // A channel outage must never hide the project warning. Save the notice before
    // posting it, retry it separately, and deduplicate a crash after the append.
    crate::state::save_now(application)?;
    for notice in notices {
        if let Some(state) = &notice.state {
            crate::airc_events::record_mirror_rejected(
                application,
                project,
                notice.sequence,
                &notice.name,
                state,
            )?;
        }
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .finish_mirror_notice(project, notice.sequence)
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn publish(
    application: &AppHandle,
    project: u32,
    head: velocity_rosen::EventHead,
    observation: Option<velocity_rosen::MirrorObservation>,
) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let (sequence, state) = observation.map_or((0, None), |item| (item.sequence, item.state));
    store
        .resume_rosen_mirror(project, &head.identity, sequence, state.as_ref())
        .map_err(|error| error.to_string())?;
    let Some(item) = store.project(project) else {
        return Ok(());
    };
    if sequence < item.rosen_mirror_sequence {
        return Ok(());
    }
    let changed = item.rosen_mirror != state;
    let toast = (item.rosen_mirror_rejection_sequence.is_none()
        && state.as_ref().is_some_and(|state| state.rejected))
    .then(|| format!("{}: Rosen mirror rejected · needs a person", item.name));
    store
        .save_rosen_mirror(project, sequence, state)
        .map_err(|error| error.to_string())?;
    if changed || toast.is_some() {
        let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
        store.refresh_layout(&mut layout);
        if let Some(message) = toast {
            layout.show_toast(message, None);
        }
        crate::layout::publish(application, &layout.snapshot(), None)?;
    }
    Ok(())
}

/// Shows or hides Open on Collapse when the mirror remote changes (#429), and Send
/// Collapse its live state when `rosen.toml` gains or loses its Collapse root (#517).
fn save_collapse_repository(
    application: &AppHandle,
    project: u32,
    repository: Option<String>,
    live_state: Option<bool>,
) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let mirrored = repository.is_some();
    let changed = store
        .save_collapse_repository(project, repository, live_state)
        .map_err(|error| error.to_string())?;
    if changed {
        let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
        store.refresh_layout(&mut layout);
        crate::layout::publish(application, &layout.snapshot(), None)?;
    }
    if mirrored {
        // A push just reached Collapse: read its land and forwarder state now (#427).
        crate::collapse_mirror::refresh_soon(application);
    }
    Ok(())
}

/// How long a project's first server start waits for its catch-up from the mirror. Rosen
/// finishes a catch-up it began even after Velocity stops waiting for it.
const CATCH_UP_WAIT: Duration = Duration::from_secs(120);

/// How long a catch-up that failed on start waits before it is tried again. Each later
/// wait doubles, up to [`CATCH_UP_RETRY_LIMIT`], because Rosen pauses landing while it
/// fetches the mirror.
const CATCH_UP_RETRY: Duration = Duration::from_secs(30);
const CATCH_UP_RETRY_LIMIT: Duration = Duration::from_secs(10 * 60);

/// Catches the project up from its mirror the first time this run readies its server
/// (#501), so the machine that sat idle lands fast-forward again with no manual step.
/// Concurrent first callers wait for the one catch-up; later calls return at once. A
/// catch-up that failed is tried again in the background until Rosen answers.
pub(crate) async fn catch_up_once(application: &AppHandle, project: u32, server: &ProjectServer) {
    let once = application
        .state::<crate::rosen::ManagedRosen>()
        .catch_up_once(project);
    once.get_or_init(|| async {
        let Err(error) = catch_up_within_wait(application, project, server).await else {
            return;
        };
        report_failed_catch_up(application, project, &error);
        application
            .state::<crate::rosen::ManagedRosen>()
            .begin_catch_up_retry(project);
        let (handle, server) = (application.clone(), server.clone());
        tauri::async_runtime::spawn(async move {
            retry_catch_up(&handle, project, &server, error).await;
        });
    })
    .await;
}

async fn catch_up_within_wait(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
) -> Result<(), String> {
    let caught_up = catch_up(application, project, server, false);
    match tokio::time::timeout(CATCH_UP_WAIT, caught_up).await {
        Ok(result) => result,
        Err(_) => Err("still running after two minutes; Rosen finishes it".into()),
    }
}

/// Says once that the catch-up on start failed. A land before one gets through goes out
/// on a base behind the mirror, and the two histories then diverge.
fn report_failed_catch_up(application: &AppHandle, project: u32, error: &str) {
    crate::diagnostics::record(
        "rosen mirror",
        &format!("Catch-up from the mirror for project {project}: {error}"),
    );
    report(project_name(application, project).and_then(|name| {
        let message = format!("{name}: could not catch up from the mirror · trying again");
        toast(application, message)
    }));
}

/// Tries a catch-up that failed on start again until Rosen answers one, here or for the
/// project's action, or the project leaves Rosen. A mirror fetch that failed once, with
/// the network not up yet, would otherwise leave the base behind its mirror all run.
async fn retry_catch_up(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    mut last_error: String,
) {
    let mut wait = CATCH_UP_RETRY;
    loop {
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(CATCH_UP_RETRY_LIMIT);
        let retries = || {
            application
                .state::<crate::rosen::ManagedRosen>()
                .retries_catch_up(project)
        };
        if retries() && !crate::rosen::rosen_in_use(application, project) {
            application
                .state::<crate::rosen::ManagedRosen>()
                .end_catch_up_retry(project);
        }
        if !retries() {
            return;
        }
        // Connecting starts a stopped server, which an update or an exit must not find running.
        if crate::rosen::paused(application) {
            continue;
        }
        match catch_up_within_wait(application, project, server).await {
            // Rosen answered, and `catch_up` ended the retries and said what happened.
            Ok(()) => return,
            Err(error) if error != last_error => {
                crate::diagnostics::record(
                    "rosen mirror",
                    &format!("Catch-up from the mirror for project {project}: {error}"),
                );
                last_error = error;
            }
            // The same failure again is already in the log.
            Err(_) => {}
        }
    }
}

/// What a health-triggered mirror push retry / catch-up did (#509).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MirrorRetryOutcome {
    UpToDate,
    RetriedPush,
    CaughtUp {
        commits: usize,
        new_base: String,
    },
    Diverged {
        base_commit: String,
        mirror_commit: String,
    },
    NotApplicable(String),
}

/// Retries a stalled mirror push or applies #501 catch-up when the mirror is ahead (#509).
/// Rosen's `mirror_catch_up` fetches the mirror, wakes its push worker when the base is
/// ahead, fast-forwards when the mirror is ahead, or returns both tips on divergence.
pub(crate) async fn retry_for_health(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
) -> Result<MirrorRetryOutcome, String> {
    let outcome = velocity_rosen::catch_up_from_mirror(server)
        .await
        .map_err(|error| error.to_string())?;
    let name = project_name(application, project)?;
    let (divergence, toast_message, result) = match outcome {
        CatchUp::NotApplicable(reason) => (None, None, MirrorRetryOutcome::NotApplicable(reason)),
        CatchUp::UpToDate => (None, None, MirrorRetryOutcome::UpToDate),
        CatchUp::Ahead => (None, None, MirrorRetryOutcome::RetriedPush),
        CatchUp::FastForwarded(moved) => {
            for failure in &moved.rebase_errors {
                crate::diagnostics::record("rosen mirror", &format!("{name}: {failure}"));
            }
            let message = format!(
                "{name}: caught up {} commit(s) from the mirror to {}",
                moved.commits,
                short(&moved.new_base)
            );
            let result = MirrorRetryOutcome::CaughtUp {
                commits: moved.commits,
                new_base: moved.new_base,
            };
            (None, Some(message), result)
        }
        CatchUp::Diverged(divergence) => {
            let result = MirrorRetryOutcome::Diverged {
                base_commit: divergence.base_commit.clone(),
                mirror_commit: divergence.mirror_commit.clone(),
            };
            let message = format!("{name}: Rosen base and mirror have diverged · needs a person");
            (Some(divergence), Some(message), result)
        }
    };
    let changed = publish_divergence(application, project, divergence, None)?;
    if let Some(message) = toast_message
        && (changed || matches!(result, MirrorRetryOutcome::CaughtUp { .. }))
    {
        let _ = toast(application, message);
    }
    let _ = refresh(application, project).await;
    Ok(result)
}

/// `catch_up_from_mirror` (§3.1): the project's Catch up from mirror action. Says what
/// happened in a toast, and shows a divergence in the project's mirror notice.
#[tauri::command]
#[specta::specta]
pub async fn catch_up_from_mirror(
    application: AppHandle,
    project_identifier: u32,
) -> Result<(), String> {
    let server = crate::rosen::server(&application, project_identifier).await?;
    catch_up(&application, project_identifier, &server, true).await?;
    refresh(&application, project_identifier).await
}

/// `adopt_mirror_tip` (§3.1): adopts the diverged mirror tip the project's notice shows,
/// for a mirror whose edits were already ported into Rosen (Rosen #396). Rust takes the
/// tip from the saved divergence; Rosen refuses a tip that moved or was not ported.
#[tauri::command]
#[specta::specta]
pub async fn adopt_mirror_tip(
    application: AppHandle,
    project_identifier: u32,
) -> Result<(), String> {
    let (name, divergence) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let item = store.project(project_identifier).ok_or("Unknown project")?;
        (item.name.clone(), item.rosen_mirror_divergence.clone())
    };
    let divergence = divergence.ok_or("This project's base and mirror have not diverged")?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    let base = velocity_rosen::adopt_mirror_tip(&server, &divergence.mirror_commit)
        .await
        .map_err(|error| error.to_string())?;
    let message = format!(
        "{name}: adopted mirror tip {} · Rosen base is now {}",
        short(&divergence.mirror_commit),
        short(&base)
    );
    publish_divergence(&application, project_identifier, None, Some(message))?;
    refresh(&application, project_identifier).await
}

/// Catches up, saves or clears the divergence, and toasts what changed. `asked` is the
/// project's action, which always says what happened; startup speaks only when the base
/// moved, a new divergence needs a person, or it had said that its catch-up failed.
async fn catch_up(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    asked: bool,
) -> Result<(), String> {
    let outcome = velocity_rosen::catch_up_from_mirror(server)
        .await
        .map_err(|error| error.to_string())?;
    // Rosen answered, so a catch-up that failed on start needs no further tries, and the
    // owner who was told it failed is told how it ended.
    let asked = application
        .state::<crate::rosen::ManagedRosen>()
        .end_catch_up_retry(project)
        || asked;
    let name = project_name(application, project)?;
    if let CatchUp::FastForwarded(moved) = &outcome {
        for failure in &moved.rebase_errors {
            crate::diagnostics::record("rosen mirror", &format!("{name}: {failure}"));
        }
    }
    let (divergence, message, moved) = describe(&name, outcome);
    let diverged = divergence.clone();
    let changed = publish_divergence(application, project, divergence, None)?;
    if asked || moved || (diverged.is_some() && changed) {
        toast(application, message)?;
    }
    if let Some(divergence) = diverged.filter(|_| changed) {
        crate::diagnostics::record("rosen mirror", &format!("{name}: {}", divergence.detail));
    }
    Ok(())
}

/// The divergence to keep, the toast, and whether the base moved.
fn describe(name: &str, outcome: CatchUp) -> (Option<MirrorDivergence>, String, bool) {
    match outcome {
        CatchUp::NotApplicable(reason) => (
            None,
            format!("{name}: nothing to catch up · {reason}"),
            false,
        ),
        CatchUp::UpToDate => (
            None,
            format!("{name}: Rosen base matches its mirror"),
            false,
        ),
        CatchUp::Ahead => (
            None,
            format!("{name}: Rosen base is ahead of its mirror · the next push sends it"),
            false,
        ),
        CatchUp::FastForwarded(moved) => {
            let mut message = format!(
                "{name}: caught up {} commit(s) from the mirror to {} · {} open change(s) rebased",
                moved.commits,
                short(&moved.new_base),
                moved.rebased
            );
            if moved.conflicted > 0 {
                message.push_str(&format!(", {} with conflicts", moved.conflicted));
            }
            if !moved.rebase_errors.is_empty() {
                let count = moved.rebase_errors.len();
                message.push_str(&format!(" · {count} rebase when they land"));
            }
            (None, message, true)
        }
        CatchUp::Diverged(divergence) => (
            Some(divergence),
            format!("{name}: Rosen base and mirror have diverged · needs a person"),
            false,
        ),
    }
}

/// Saves the divergence and republishes the window when it changed or `message` is a
/// toast to show. Reports whether the divergence changed.
fn publish_divergence(
    application: &AppHandle,
    project: u32,
    divergence: Option<MirrorDivergence>,
    message: Option<String>,
) -> Result<bool, String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let changed = store
        .save_mirror_divergence(project, divergence)
        .map_err(|error| error.to_string())?;
    if changed || message.is_some() {
        let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
        store.refresh_layout(&mut layout);
        if let Some(message) = message {
            layout.show_toast(message, None);
        }
        crate::layout::publish(application, &layout.snapshot(), None)?;
    }
    Ok(changed)
}

fn project_name(application: &AppHandle, project: u32) -> Result<String, String> {
    Ok(lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project)
    .map(|item| item.name.clone())
    .unwrap_or_default())
}

fn toast(application: &AppHandle, message: String) -> Result<(), String> {
    let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
    layout.show_toast(message, None);
    crate::layout::publish(application, &layout.snapshot(), None)
}

/// A commit's first twelve characters, as toasts name it.
fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
