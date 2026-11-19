//! Adapt Rosen's mirror observation to saved project state, the window and AIRC.

use tauri::{AppHandle, Manager};

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
    save_collapse_repository(
        application,
        project,
        remote
            .as_deref()
            .and_then(velocity_core::collapse::repository_from_remote),
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

/// Shows or hides Open on Collapse when the mirror remote changes (#429).
fn save_collapse_repository(
    application: &AppHandle,
    project: u32,
    repository: Option<String>,
) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let changed = store
        .save_collapse_repository(project, repository)
        .map_err(|error| error.to_string())?;
    if changed {
        let mut layout = lock(application.state::<MainLayout>().inner(), "layout")?;
        store.refresh_layout(&mut layout);
        crate::layout::publish(application, &layout.snapshot(), None)?;
    }
    Ok(())
}
