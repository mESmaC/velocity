//! F14 event log posted by Velocity from GitHub, scouts, threads and the Task List,
//! with agent messages and ledgers kept separate (`SPEC.md §3.1`, `§4 F14`).
use tauri::{AppHandle, Manager};
use tauri_specta::Event as SpectaEvent;
use velocity_airc::{Channel, Entry, Event};
use velocity_core::duty::{DutyEventKind, DutyFeedEvent};
use velocity_github::{PullRequestSnapshot, follow_through::FollowThrough};
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

/// Posts F6/F7 PR lifecycle events (`PR`, `pushed`, `merged` + Past work ledger)
/// observed on `snapshot` relative to `before`.
pub(crate) fn record_pull_request_observation(
    application: &AppHandle,
    project_identifier: u32,
    thread: &ThreadItem,
    before: Option<&FollowThrough>,
    after: &FollowThrough,
    snapshot: &PullRequestSnapshot,
) {
    if after.number == 0 {
        return;
    }
    let nick = thread_nick(thread);
    let number = after.number;
    let title = snapshot.pull_request.title.as_str();
    let head = after.head.as_str();
    let opened = before.is_none_or(|previous| previous.number == 0);
    if opened {
        report(
            Event::pull_request_opened(&nick, number, title, head)
                .map_err(|error| error.to_string())
                .and_then(|event| post_event(application, project_identifier, event)),
        );
    } else if before.is_some_and(|previous| !previous.head.is_empty() && previous.head != head) {
        report(
            Event::pull_request_pushed(&nick, number, head)
                .map_err(|error| error.to_string())
                .and_then(|event| post_event(application, project_identifier, event)),
        );
    }
    record_proved(application, project_identifier, &nick, before, after);
    if after.stage == "merged" && after.worktree_deleted {
        // `master` moved: ready worktrees catch up and rebuild at the new tip (F5).
        crate::ready_pool::wake(application, Some(project_identifier));
        report(record_merged(
            application,
            project_identifier,
            &nick,
            number,
            title,
            head,
        ));
    }
}

/// Posts `proved` (F14, F17) when Velocity's proof checks first pass at a PR head, naming
/// the proof folders, the theorem count and each language's level.
fn record_proved(
    application: &AppHandle,
    project_identifier: u32,
    nick: &str,
    before: Option<&FollowThrough>,
    after: &FollowThrough,
) {
    let already = before.is_some_and(|previous| previous.proved() && previous.head == after.head);
    let Some(card) = after.proof.card.as_ref().filter(|_| after.proved()) else {
        return;
    };
    if already {
        return;
    }
    let text = format!("proved #{} · {}", after.number, card.summary());
    let event = Event::new("proved", nick, &text).for_pull_request(after.number, Some(&after.head));
    report(post_event(application, project_identifier, event));
}

fn record_merged(
    application: &AppHandle,
    project_identifier: u32,
    nick: &str,
    number: u32,
    title: &str,
    head: &str,
) -> Result<(), String> {
    let channel = open_channel(application, project_identifier)?;
    if channel
        .events(0)
        .map_err(|error| error.to_string())?
        .iter()
        .any(|entry| entry.kind == "merged" && entry.pull_request == Some(number))
    {
        return Ok(());
    }
    let files = merged_files(application, project_identifier, number)?;
    if let Some(entry) = channel
        .post_pull_request_merged_with_files(nick, number, title, head, files)
        .map_err(|error| error.to_string())?
    {
        AircEventPublished {
            project_identifier,
            entry,
        }
        .emit(application)
        .map_err(|error| error.to_string())?;
    }
    Ok(())
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
pub fn airc_events(
    application: AppHandle,
    project_identifier: u32,
    after: Option<u32>,
) -> Result<Vec<Entry>, String> {
    open_channel(&application, project_identifier)?
        .events(cursor(after))
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn airc_messages(
    application: AppHandle,
    project_identifier: u32,
    after: Option<u32>,
) -> Result<Vec<Entry>, String> {
    open_channel(&application, project_identifier)?
        .messages(cursor(after))
        .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn airc_ledger(
    application: AppHandle,
    project_identifier: u32,
    ledger: String,
    after: Option<u32>,
) -> Result<Vec<Entry>, String> {
    open_channel(&application, project_identifier)?
        .ledger(&ledger, cursor(after))
        .map_err(|error| error.to_string())
}

fn merged_files(application: &AppHandle, project: u32, number: u32) -> Result<Vec<String>, String> {
    let repository = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project)
    .map(|project| project.repository_path.clone())
    .ok_or("Unknown project")?;
    velocity_github::PullRequestClient::discover(std::path::Path::new(&repository))
        .and_then(|client| client.changed_files(number))
        .map_err(|error| error.to_string())
}
