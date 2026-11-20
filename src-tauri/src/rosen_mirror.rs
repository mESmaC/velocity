//! Adapt Rosen's mirror observation to saved project state, the window and AIRC, and
//! catch projects up from their mirror (#501).

use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_core::rosen_mirror::MirrorDivergence;
use velocity_rosen::{CatchUp, ProjectServer};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::lock;

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

/// Catches the project up from its mirror the first time this run readies its server
/// (#501), so the machine that sat idle lands fast-forward again with no manual step.
/// Concurrent first callers wait for the one catch-up; later calls return at once.
pub(crate) async fn catch_up_once(application: &AppHandle, project: u32, server: &ProjectServer) {
    let once = application
        .state::<crate::rosen::ManagedRosen>()
        .catch_up_once(project);
    once.get_or_init(|| async {
        let caught_up = catch_up(application, project, server, false);
        let result = match tokio::time::timeout(CATCH_UP_WAIT, caught_up).await {
            Ok(result) => result,
            Err(_) => Err("still running after two minutes; Rosen finishes it".into()),
        };
        if let Err(error) = result {
            crate::diagnostics::record(
                "rosen mirror",
                &format!("Catch-up from the mirror for project {project}: {error}"),
            );
        }
    })
    .await;
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
/// moved or a new divergence needs a person.
async fn catch_up(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
    asked: bool,
) -> Result<(), String> {
    let outcome = velocity_rosen::catch_up_from_mirror(server)
        .await
        .map_err(|error| error.to_string())?;
    let name = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project)
    .map(|item| item.name.clone())
    .unwrap_or_default();
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

fn toast(application: &AppHandle, message: String) -> Result<(), String> {
    let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
    layout.show_toast(message, None);
    crate::layout::publish(application, &layout.snapshot(), None)
}

/// A commit's first twelve characters, as toasts name it.
fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
