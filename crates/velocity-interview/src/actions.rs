//! What you do in an interview (F3, §5): answer with `1`–`4` or your own words, Keep
//! grilling (`G`), switch proof-list entries, Lock (`L`) and open SPEC.md (`S`).

use crate::process::hidden_command;
use std::path::Path;
use std::sync::Arc;

use velocity_core::layout_view::LayoutIntent;
use velocity_core::navigation::NavigationIntent;
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::{InterviewStage, ProjectItem};

use crate::service::{self, Host, InterviewKey};
use crate::turns::{self, Answer};
use crate::{InterviewError, prompts};

/// Carries out `intent` when it belongs to the open spec interview. Returns `false` for
/// every other intent, which its own feature handles.
pub fn handle_intent<H: Host>(host: &Arc<H>, intent: &LayoutIntent) -> bool {
    let target = match intent {
        LayoutIntent::SteerThread { identifier, .. } => u32::try_from(*identifier).ok(),
        _ => service::locked_layout(host.as_ref())
            .selected_thread()
            .and_then(|identifier| u32::try_from(identifier).ok()),
    };
    let Some(key) = target.and_then(|thread| interview_key(host.as_ref(), thread)) else {
        return false;
    };
    match intent {
        LayoutIntent::Navigation(NavigationIntent::Answer(number)) => {
            answer(host, key, Answer::Option(*number))
        }
        LayoutIntent::SteerThread { prompt, .. } => reply(host, key, prompt),
        LayoutIntent::Navigation(NavigationIntent::Grill) => grill(host, key),
        LayoutIntent::ToggleProofListEntry(index) => {
            service::save_step(host.as_ref(), key.0, |project| {
                turns::toggle_proof_entry(project, key.1, *index)
            });
        }
        LayoutIntent::Navigation(NavigationIntent::Lock) => lock_spec(host, key),
        LayoutIntent::Navigation(NavigationIntent::SpecFile) => open_spec(host.as_ref(), key),
        _ => return false,
    }
    true
}

/// The open project and `thread`, when `thread` is one of its interviews.
fn interview_key<H: Host>(host: &H, thread: u32) -> Option<InterviewKey> {
    let store = service::locked_store(host);
    let project = store.active_project_identifier()?;
    let item = store
        .state()
        .projects
        .iter()
        .find(|item| item.id == project)?;
    item.threads
        .iter()
        .any(|item| item.id == thread && item.interview.is_some())
        .then_some((project, thread))
}

fn answer<H: Host>(host: &Arc<H>, key: InterviewKey, answer: Answer) {
    let recorded = service::save_step(host.as_ref(), key.0, |project| {
        turns::record_answer(project, key.1, answer)
    })
    .flatten();
    if let Some(recorded) = recorded {
        let prompt = prompts::answer_prompt(&recorded.key, &recorded.value);
        let shown = TranscriptBlock::Decision {
            question: recorded.question,
            key: recorded.key,
            value: recorded.value,
        };
        service::send(host, key, prompt, shown);
    }
}

/// Text from the composer: the answer when a question waits, a message otherwise.
fn reply<H: Host>(host: &Arc<H>, key: InterviewKey, text: &str) {
    let stage = with_project(host.as_ref(), key, |project| {
        let record = project
            .threads
            .iter()
            .find(|thread| thread.id == key.1)?
            .interview
            .as_ref()?;
        Some((record.stage, record.question.is_some()))
    });
    match stage {
        Some((InterviewStage::Locked, _)) | None => {}
        Some((InterviewStage::Ask, true)) => answer(host, key, Answer::Text(text.into())),
        Some(_) => {
            let shown = TranscriptBlock::UserMessage {
                text: text.into(),
                queued: false,
            };
            service::send(host, key, prompts::message_prompt(text), shown);
        }
    }
}

fn grill<H: Host>(host: &Arc<H>, key: InterviewKey) {
    let spec_path = with_project(host.as_ref(), key, |project| {
        Some(project.spec_path.clone())
    });
    let started = service::save_step(host.as_ref(), key.0, |project| {
        turns::begin_grill(project, key.1)
    });
    if let (Some(true), Some(spec_path)) = (started, spec_path) {
        let shown = TranscriptBlock::UserMessage {
            text: "Keep grilling".into(),
            queued: false,
        };
        service::send(host, key, prompts::grill_prompt(&spec_path), shown);
    }
}

fn with_project<H: Host, Value>(
    host: &H,
    (project, _): InterviewKey,
    read: impl FnOnce(&ProjectItem) -> Option<Value>,
) -> Option<Value> {
    let store = service::locked_store(host);
    read(
        store
            .state()
            .projects
            .iter()
            .find(|item| item.id == project)?,
    )
}

/// What Lock needs from the saved project.
struct LockPlan {
    checkout: String,
    spec_path: String,
    proof_list: Vec<String>,
}

/// Lock spec & publish tasks: writes the proof list into SPEC.md, commits it, then saves
/// the project as locked. git runs off the UI thread.
fn lock_spec<H: Host>(host: &Arc<H>, key: InterviewKey) {
    let Some(plan) = lock_plan(host.as_ref(), key) else {
        return;
    };
    let task_host = Arc::clone(host);
    host.spawn(Box::pin(async move {
        let committed = tokio::task::spawn_blocking(move || commit_spec(&plan))
            .await
            .unwrap_or_else(|error| Err(InterviewError::Stopped(error.to_string())));
        match committed {
            Ok(()) => {
                service::stop(task_host.as_ref(), key);
                service::save_step(task_host.as_ref(), key.0, |project| {
                    turns::lock(project, key.1)
                });
            }
            Err(error) => {
                service::save_step(task_host.as_ref(), key.0, |project| {
                    turns::set_notice(project, key.1, error.to_string())
                });
            }
        }
    }));
}

fn lock_plan<H: Host>(host: &H, key: InterviewKey) -> Option<LockPlan> {
    with_project(host, key, |project| {
        let record = project
            .threads
            .iter()
            .find(|thread| thread.id == key.1)?
            .interview
            .as_ref()?;
        (record.stage == InterviewStage::Ready).then(|| LockPlan {
            checkout: project.repository_path.clone(),
            spec_path: project.spec_path.clone(),
            proof_list: turns::included_proof_list(record),
        })
    })
}

fn commit_spec(plan: &LockPlan) -> Result<(), InterviewError> {
    let file = service::spec_file_path(&plan.checkout, &plan.spec_path);
    let spec = std::fs::read_to_string(&file).map_err(|source| InterviewError::NoSpec {
        path: plan.spec_path.clone(),
        source,
    })?;
    let locked = crate::spec_file::with_proof_list(&spec, &plan.proof_list);
    std::fs::write(&file, locked).map_err(|source| InterviewError::WriteSpec {
        path: plan.spec_path.clone(),
        source,
    })?;
    velocity_git::commit_file(
        Path::new(&plan.checkout),
        Path::new(&plan.spec_path),
        "Lock spec",
    )?;
    Ok(())
}

/// SPEC.md (`S`): opens the file in the app Windows associates with it.
fn open_spec<H: Host>(host: &H, key: InterviewKey) {
    let Some(file) = with_project(host, key, |project| {
        Some(service::spec_file_path(
            &project.repository_path,
            &project.spec_path,
        ))
    }) else {
        return;
    };
    let opened = if file.is_file() {
        open_with_default_app(&file).map_err(|error| error.to_string())
    } else {
        Err(String::from(
            "SPEC.md isn't written yet. The agent writes it once nothing is ambiguous.",
        ))
    };
    if let Err(message) = opened {
        service::save_step(host, key.0, |project| {
            turns::set_notice(project, key.1, message)
        });
    }
}

fn open_with_default_app(file: &Path) -> std::io::Result<()> {
    let mut command = if cfg!(windows) {
        hidden_command("explorer.exe")
    } else if cfg!(target_os = "macos") {
        hidden_command("open")
    } else {
        hidden_command("xdg-open")
    };
    // Explorer exits with 1 even when it opened the file, so only a failed start counts.
    command.arg(file).spawn().map(|_| ())
}
