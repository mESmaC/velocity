//! Tauri edge for ACP context hand-offs and Rosen outcome observations.

use agent_client_protocol::v1::schema::StopReason;
use tauri::{AppHandle, Manager};
use velocity_acp::client::{AgentEvent, HandoffExperiment, SessionResume, SessionSetup};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::{
    AgentActivity, ThreadItem,
    handoff::{can_handoff, note_for},
};
use velocity_threads::{
    ThreadKey,
    parking::{HANDOFF_BOUNDARY, transcript_context},
};

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock, report};

pub(crate) fn configure(application: &AppHandle, key: ThreadKey, setup: &mut SessionSetup) {
    let result = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .and_then(|mut store| {
        store
            .enroll_handoff(key.0, key.1)
            .map_err(|error| error.to_string())
    });
    match result {
        Ok(item) => {
            setup.handoff = item.map(|(change, enabled)| HandoffExperiment::new(change, enabled))
        }
        Err(error) => report::<()>(Err(error)),
    }
}

pub(crate) fn resume_context(
    thread: &ThreadItem,
    blocks: &[TranscriptBlock],
) -> Option<SessionResume> {
    let note = note_for(thread);
    if note.is_some_and(|note| note.pending) {
        return None;
    }
    let transcript = match note {
        Some(note) => {
            let after = blocks.iter().rposition(|block| {
                matches!(block, TranscriptBlock::Notice { text } if text == HANDOFF_BOUNDARY)
            }).map_or(blocks.len(), |position| position + 1);
            format!(
                "Saved hand-off for this work item:\n\n{}\n\n{}",
                note.text,
                transcript_context(&blocks[after..])
            )
        }
        None => transcript_context(blocks),
    };
    Some(
        SessionResume::new(thread.agent_session.clone(), transcript)
            .with_history_replay(blocks.is_empty() && note.is_none()),
    )
}

pub(crate) async fn add_preface(application: &AppHandle, key: ThreadKey, setup: &mut SessionSetup) {
    let note = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        let project = store.project(key.0)?;
        let thread = project.threads.iter().find(|thread| thread.id == key.1)?;
        note_for(thread).filter(|note| note.pending).cloned()
    });
    let Some(note) = note else {
        return;
    };
    let state = match application.path().app_data_dir() {
        Ok(data) => {
            let server = velocity_rosen::ProjectServer::new(&data, key.0);
            velocity_rosen::dispatch::handoff_state(&server, &note.change)
                .await
                .map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
    .unwrap_or_else(|error| {
        format!(
            "Rosen change {}: state query failed ({error}). Read rosen status before continuing.",
            note.change
        )
    });
    setup.handoff_context = Some(format!(
        "Saved hand-off for this work item:\n\n{}\n\nCurrent change state:\n\n{state}",
        note.text
    ));
}

#[derive(Default)]
pub(crate) struct Progress {
    pub accepted: bool,
    replacement_started: bool,
}

pub(crate) fn observe(
    application: &AppHandle,
    key: ThreadKey,
    batch: &[AgentEvent],
    progress: &mut Progress,
) -> Result<(), String> {
    for event in batch {
        let store = application.state::<ManagedStateStore>();
        match event {
            AgentEvent::HandoffThreshold { change, decision } => {
                let result = observe_threshold(application, key, change);
                if let Some(decision) = decision {
                    decision.answer(result.as_ref().is_ok_and(|allowed| *allowed));
                }
                result?;
            }
            AgentEvent::HandoffReady {
                change,
                note,
                decision,
            } => {
                let accepted = accept(application, key, change, note);
                decision.answer(matches!(accepted, Ok(true)));
                progress.accepted = accepted?;
            }
            AgentEvent::SessionStarted { .. } if pending(application, key) => {
                progress.replacement_started = true;
                lock(
                    application.state::<ManagedThreads>().inner(),
                    "thread sessions",
                )?
                .handoff_boundary(key);
            }
            AgentEvent::TurnEnded(Ok(StopReason::EndTurn))
                if !progress.accepted && progress.replacement_started =>
            {
                lock(store.inner(), "persisted state")?
                    .complete_handoff(key)
                    .map_err(|error| error.to_string())?;
                progress.replacement_started = false;
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn reject(batch: &[AgentEvent]) {
    for event in batch {
        match event {
            AgentEvent::HandoffThreshold {
                decision: Some(decision),
                ..
            }
            | AgentEvent::HandoffReady { decision, .. } => decision.answer(false),
            _ => {}
        }
    }
}

fn observe_threshold(
    application: &AppHandle,
    key: ThreadKey,
    change: &str,
) -> Result<bool, String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    store
        .observe_handoff(key, change, None)
        .map_err(|error| error.to_string())?;
    let Ok((_, mut thread)) = store.follow_through_thread(key.0, key.1) else {
        return Ok(false);
    };
    // The just-finished logical turn is checkpointed after the summary.
    thread.supervision.turns = thread.supervision.turns.saturating_add(1);
    let settings = store.settings();
    let capped = velocity_threads::agent_activity::capped(
        &thread,
        &settings.agents,
        u64::from(velocity_store::providers::unix_now()),
    );
    Ok(can_handoff(&thread) && !capped && thread.rosen_change.as_deref() == Some(change))
}

fn accept(
    application: &AppHandle,
    key: ThreadKey,
    change: &str,
    text: &str,
) -> Result<bool, String> {
    let managed = application.state::<ManagedStateStore>();
    let (accepted, previous) = {
        let mut store = lock(managed.inner(), "persisted state")?;
        let previous = store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
            .and_then(|thread| thread.context_handoff.clone());
        let accepted = store
            .observe_handoff(key, change, Some(text))
            .map_err(|error| error.to_string())?;
        (accepted, previous)
    };
    if accepted {
        if let Err(error) = crate::state::save_now(application) {
            let _ =
                lock(managed.inner(), "persisted state")?.update_thread(key.0, key.1, |thread| {
                    if thread.rosen_change.as_deref() == Some(change) {
                        thread.context_handoff = previous;
                    }
                });
            return Err(error);
        }
        lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?
        .reserve_handoff(key);
    }
    Ok(accepted)
}

fn pending(application: &AppHandle, key: ThreadKey) -> bool {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .is_ok_and(|store| {
        store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
            .and_then(note_for)
            .is_some_and(|note| note.pending)
    })
}

pub(crate) fn resume(application: &AppHandle, key: ThreadKey) {
    if let Err(error) = resume_if_current(application, key) {
        crate::threads::fail(
            application,
            key,
            "Could not continue the context hand-off",
            &error,
        );
    }
}

fn resume_if_current(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let lease = crate::threads::action_lock(application, key)?;
    let _guard = lease.blocking_lock();
    if lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .is_live(key)
        || lock(
            application
                .state::<crate::thread_menu::ManagedDeletions>()
                .inner(),
            "pending deletions",
        )?
        .is_pending(key)
    {
        return Ok(());
    }
    let should_resume = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let Ok((_, thread)) = store.follow_through_thread(key.0, key.1) else {
            return Ok(());
        };
        can_handoff(&thread)
            && note_for(&thread).is_some_and(|note| note.pending)
            && !matches!(
                thread.activity,
                AgentActivity::Parked | AgentActivity::AtLimit
            )
    };
    if should_resume {
        crate::parking::resume_handoff(application, key)?;
    }
    Ok(())
}

pub(crate) fn observe_review(
    application: &AppHandle,
    project: u32,
    event: &velocity_rosen::ProjectEvent,
) -> Result<(), String> {
    let Some(change) = &event.change else {
        return Ok(());
    };
    let detail = &event.detail;
    let (target, verdict) = match detail["action"].as_str() {
        Some("approve") => (
            detail["head"].as_str().map(|head| format!("head:{head}")),
            Some("approved"),
        ),
        Some("mark") => (
            detail["commit"]
                .as_str()
                .map(|commit| format!("land:{commit}")),
            detail["state"]
                .as_str()
                .filter(|state| *state != "unreviewed"),
        ),
        _ => return Ok(()),
    };
    if let Some(target) = target {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .observe_handoff_review(project, change, target, verdict)
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}
