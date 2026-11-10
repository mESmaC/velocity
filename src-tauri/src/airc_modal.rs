//! AIRC modal edge (F14). Agents write the channel from their own MCP processes, so the
//! app reads the active project's channel once a second: it publishes the footer badge
//! on `layout://state` and the modal on `airc://modal`, and marks lines read while the
//! modal is open. The window never polls.
use std::sync::Mutex;

use tauri::{AppHandle, Manager};
use tauri_specta::Event as SpectaEvent;
use velocity_airc::{Channel, Entry};
use velocity_core::airc_modal::{
    AircClaimRecord, AircModalInput, AircModalView, AircRecord, latest_sequence, modal_view,
    unread_badge,
};
use velocity_store::airc_members::airc_members;
use velocity_store::projects::ProjectError;

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::lock;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, SpectaEvent)]
#[tauri_specta(event_name = "airc://modal")]
pub struct AircModalPublished {
    pub project_identifier: u32,
    pub view: AircModalView,
}

/// The last view published, so an unchanged channel sends nothing.
#[derive(Default)]
pub struct ManagedAircModal(Mutex<Option<AircModalPublished>>);

struct ActiveChannel {
    project: u32,
    seen: u64,
    input: AircModalInput,
}

pub fn initialize(application: &AppHandle) {
    let application = application.clone();
    std::thread::spawn(move || {
        loop {
            if let Err(error) = refresh(&application) {
                eprintln!("Velocity AIRC modal: {error}");
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

/// Reads the channel now; opening the modal and sending a comment call this so neither
/// waits for the next tick.
pub(crate) fn refresh(application: &AppHandle) -> Result<(), String> {
    let Some(active) = read_active(application)? else {
        return Ok(());
    };
    let open = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?
    .airc_open();
    let mut seen = active.seen;
    let latest = latest_sequence(&active.input.records);
    if open && latest > seen {
        seen = latest;
        mark_seen(application, active.project, seen)?;
    }
    let badge = unread_badge(&active.input.records, seen);
    {
        let mut layout = lock(
            application.state::<MainLayout>().inner(),
            "main-window state",
        )?;
        if layout.set_airc_badge(active.project, badge) {
            crate::layout::publish(application, &layout.snapshot(), None)?;
        }
    }
    // A closed modal only needs the badge; opening it fetches the view.
    if !open {
        return Ok(());
    }
    let published = AircModalPublished {
        project_identifier: active.project,
        view: modal_view(&active.input),
    };
    let state = application.state::<ManagedAircModal>();
    let mut last = lock(&state.inner().0, "AIRC modal")?;
    if last.as_ref().is_some_and(|last| {
        last.project_identifier == published.project_identifier && last.view == published.view
    }) {
        return Ok(());
    }
    published
        .emit(application)
        .map_err(|error| error.to_string())?;
    *last = Some(published);
    Ok(())
}

/// The modal's current view, for the window to draw before the next `airc://modal`.
#[tauri::command]
#[specta::specta]
pub fn get_airc_modal(
    application: AppHandle,
    project_identifier: u32,
) -> Result<AircModalView, String> {
    let active = read_active(&application)?
        .filter(|active| active.project == project_identifier)
        .ok_or("That project is not the active project")?;
    Ok(modal_view(&active.input))
}

fn read_active(application: &AppHandle) -> Result<Option<ActiveChannel>, String> {
    let (project, name, seen, members) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let state = store.state();
        let Some(project) = usize::try_from(state.active_project_index)
            .ok()
            .and_then(|index| state.projects.get(index))
        else {
            return Ok(None);
        };
        (
            project.id,
            project.name.clone(),
            project.airc_seen,
            airc_members(project),
        )
    };
    let directory = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let entries = Channel::open(&directory, &project.to_string())
        .and_then(|channel| channel.read(0))
        .map_err(|error| error.to_string())?;
    let claims = velocity_airc::claims::claims(&entries)
        .into_iter()
        .map(|claim| AircClaimRecord::new(&claim.member, &claim.nick, &claim.task, claim.files))
        .collect();
    Ok(Some(ActiveChannel {
        project,
        seen,
        input: AircModalInput {
            channel: name,
            records: entries.iter().map(record).collect(),
            claims,
            members,
        },
    }))
}

fn record(entry: &Entry) -> AircRecord {
    let shown = entry.as_event_entry();
    AircRecord::new(
        entry.sequence,
        &entry.time,
        &entry.kind,
        &entry.member,
        &entry.nick,
    )
    .with_text(
        &entry
            .message_mode
            .map(|mode| format!("{mode:?}"))
            .unwrap_or_else(|| entry.text.clone()),
        entry.to.clone(),
    )
    .with_event_text(&shown.text)
    .as_event(entry.is_event())
}

fn mark_seen(application: &AppHandle, project: u32, seen: u64) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_project(project, |item| {
        item.airc_seen = seen;
        Ok::<_, ProjectError>(())
    })
    .map_err(|error| error.to_string())
}

/// User-owned delivery mode, independent of the agent's tool permissions.
#[tauri::command]
#[specta::specta]
pub fn set_airc_message_mode(
    application: AppHandle,
    project_identifier: u32,
    nick: String,
    mode: velocity_airc::MessageMode,
) -> Result<(), String> {
    read_active(&application)?
        .filter(|active| active.project == project_identifier)
        .ok_or("That project is not the active project")?;
    let directory = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    Channel::open(&directory, &project_identifier.to_string())
        .and_then(|channel| channel.set_message_mode(&nick, mode))
        .map_err(|error| error.to_string())?;
    refresh(&application)
}
