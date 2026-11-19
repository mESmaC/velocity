//! F14 event log posted by Velocity from GitHub, scouts, threads and the Task List,
//! with agent messages and ledgers kept separate (`SPEC.md §3.1`, `§4 F14`).
use tauri::{AppHandle, Manager};
use tauri_specta::Event as SpectaEvent;
use velocity_airc::{Channel, Entry, Event};
use velocity_core::duty::{DutyEventKind, DutyFeedEvent};
use velocity_store::ThreadItem;

use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, SpectaEvent)]
#[tauri_specta(event_name = "airc://event")]
pub struct AircEventPublished {
    pub project_identifier: u32,
    pub entry: Entry,
}

impl AircEventPublished {
    pub fn new(project_identifier: u32, entry: Entry) -> Self {
        Self {
            project_identifier,
            entry,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, SpectaEvent)]
#[tauri_specta(event_name = "airc://message")]
pub struct AircMessagePublished {
    pub project_identifier: u32,
    pub entry: Entry,
}

fn open_channel(application: &AppHandle, project_identifier: u32) -> Result<Channel, String> {
    let exists = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .state()
    .projects
    .iter()
    .any(|project| project.id == project_identifier);
    if !exists {
        return Err(format!("Project {project_identifier} is not registered"));
    }
    let app_data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    Channel::open(&app_data, &project_identifier.to_string()).map_err(|error| error.to_string())
}

fn thread_nick(thread: &ThreadItem) -> String {
    thread
        .worktree_path
        .as_deref()
        .filter(|path| !path.trim().is_empty())
        .or_else(|| (!thread.branch_name.trim().is_empty()).then_some(thread.branch_name.as_str()))
        .unwrap_or(thread.title.as_str())
        .to_owned()
}

pub(crate) fn post_event(
    application: &AppHandle,
    project_identifier: u32,
    event: Event,
) -> Result<Option<Entry>, String> {
    let channel = open_channel(application, project_identifier)?;
    let posted = channel
        .post_event_once(event)
        .map_err(|error| error.to_string())?;
    if let Some(entry) = &posted {
        AircEventPublished {
            project_identifier,
            entry: entry.clone(),
        }
        .emit(application)
        .map_err(|error| error.to_string())?;
    }
    Ok(posted)
}

/// One channel entry per rejection episode, also delivered to current duty agents.
pub(crate) fn record_mirror_rejected(
    application: &AppHandle,
    project: u32,
    sequence: u64,
    name: &str,
    state: &velocity_core::rosen_mirror::MirrorState,
) -> Result<(), String> {
    let channel = open_channel(application, project)?;
    let recipients = channel
        .presence()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|member| member.joined && member.member.starts_with("duty-"))
        .map(|member| member.nick)
        .collect();
    let last_pushed = state
        .last_pushed_commit
        .as_deref()
        .unwrap_or("no accepted commit recorded");
    let message = format!(
        "{name}: Rosen mirror rejected · needs a person (mirror event {sequence}). Last mirrored commit: {last_pushed}.\n{}",
        state.error,
    );
    let event = Event::new("failed", "velocity", &message).addressed_to(recipients);
    post_event(application, project, event).map(|_| ())
}

/// Posts an `issue` event when a scout files an issue during a sweep (F7, F14).
pub(crate) fn record_scout_events(
    application: &AppHandle,
    project_identifier: u32,
    events: &[DutyFeedEvent],
) {
    for feed_event in events {
        if feed_event.kind != DutyEventKind::File {
            continue;
        }
        if let Some((number, title, evidence)) = parse_filed_issue(&feed_event.message) {
            report(
                Event::issue_filed(&feed_event.actor, number, title)
                    .map_err(|error| error.to_string())
                    .and_then(|event| post_event(application, project_identifier, event)),
            );
            // The scout's failing script is the issue's evidence (F17): a `stress` event.
            if let Some(evidence) = evidence {
                let text = format!("{evidence} fails · evidence for #{number} {title}");
                report(post_event(
                    application,
                    project_identifier,
                    Event::new("stress", &feed_event.actor, &text),
                ));
            }
        }
    }
}

/// `Filed #<n> <title> · failing evidence attached[: <script>]`, as the scout runners write it.
fn parse_filed_issue(message: &str) -> Option<(u32, &str, Option<&str>)> {
    let rest = message.strip_prefix("Filed #")?;
    let (number_text, tail) = rest.split_once(' ')?;
    let number = number_text.parse::<u32>().ok()?;
    let (title, evidence) = match tail.rsplit_once(" · failing evidence attached") {
        Some((title, suffix)) => (title, suffix.strip_prefix(": ").map(str::trim)),
        None => (tail, None),
    };
    let title = title.trim();
    (!title.is_empty()).then_some((number, title, evidence.filter(|path| !path.is_empty())))
}

/// Posts a `failed` event when a thread stops on an error (§2.1, F14).
pub(crate) fn record_thread_failed(
    application: &AppHandle,
    project_identifier: u32,
    thread_identifier: u32,
    headline: &str,
    detail: &str,
) {
    let nick = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        store
            .project(project_identifier)
            .and_then(|project| {
                project
                    .threads
                    .iter()
                    .find(|thread| thread.id == thread_identifier)
            })
            .map(thread_nick)
    });
    let Some(nick) = nick else {
        return;
    };
    let reason = if detail.trim().is_empty() {
        headline.to_owned()
    } else {
        format!("{headline}: {detail}")
    };
    report(
        Event::thread_failed(&nick, &reason)
            .map_err(|error| error.to_string())
            .and_then(|event| post_event(application, project_identifier, event)),
    );
}

fn cursor(after: Option<u32>) -> u64 {
    u64::from(after.unwrap_or(0))
}

#[tauri::command]
#[specta::specta]
pub async fn airc_events(
    application: AppHandle,
    project_identifier: u32,
    after: Option<u32>,
) -> Result<Vec<Entry>, String> {
    crate::layout::off_ui_thread(move || {
        open_channel(&application, project_identifier)?
            .events(cursor(after))
            .map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn airc_messages(
    application: AppHandle,
    project_identifier: u32,
    after: Option<u32>,
) -> Result<Vec<Entry>, String> {
    crate::layout::off_ui_thread(move || {
        open_channel(&application, project_identifier)?
            .messages(cursor(after))
            .map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn airc_ledger(
    application: AppHandle,
    project_identifier: u32,
    ledger: String,
    after: Option<u32>,
) -> Result<Vec<Entry>, String> {
    crate::layout::off_ui_thread(move || {
        open_channel(&application, project_identifier)?
            .ledger(&ledger, cursor(after))
            .map_err(|error| error.to_string())
    })
    .await
}
