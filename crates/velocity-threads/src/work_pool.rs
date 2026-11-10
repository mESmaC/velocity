//! Read-only F16 work inventory from the existing project, GitHub and spec sources.
use std::path::Path;
use thiserror::Error;
use velocity_core::work_pool::{WorkKey, WorkPiece, WorkPool, spec_pieces};
use velocity_github::{IssueClient, IssueState};
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::{ProjectItem, TaskState, ThreadState, extended_windows_path};

#[derive(Debug, Error)]
pub enum WorkPoolError {
    #[error("Could not read SPEC.md: {0}")]
    Spec(#[from] std::io::Error),
    #[error(transparent)]
    GitHub(#[from] velocity_github::PullRequestError),
}

/// No selection or claim happens here: callers must refresh and atomically claim
/// before starting work. Failure to read a source fails the snapshot, never shows
/// a partial pool as if it were complete.
pub fn read_work_pool(project: &ProjectItem) -> Result<WorkPool, WorkPoolError> {
    if !project.spec_locked {
        return Ok(WorkPool::new(project.id, Vec::new()));
    }
    let mut pieces = task_pieces(project);
    let checkout = Path::new(&project.repository_path);
    if IssueClient::has_origin(checkout)? {
        let inventory = IssueClient::discover(checkout)?.inventory()?;
        for issue in inventory
            .issues
            .iter()
            .filter(|issue| issue.state == IssueState::Open)
        {
            let assignment = ThreadIssue::new(inventory.repository.clone(), issue.number);
            let owner = issue_thread(project, &assignment).map(|thread| thread.id);
            pieces.push(WorkPiece::new(
                WorkKey::Issue {
                    repository: inventory.repository.to_lowercase(),
                    number: issue.number,
                },
                issue.title.clone(),
                issue.url.clone(),
                owner,
            ));
        }
    }
    let path = checkout.join(&project.spec_path);
    let markdown = std::fs::read_to_string(extended_windows_path(&path))?;
    pieces.extend(available_spec_pieces(project, &markdown));
    for piece in &mut pieces {
        let prior = project.threads.iter().rev().find(|thread| {
            thread.supervision.released
                && thread.supervision.pushed_commit.is_some()
                && velocity_store::claim_piece::thread_claim_piece(thread, &thread.branch_name)
                    == piece.key.claim_piece()
        });
        if let Some(thread) = prior {
            piece.reference.push_str(&format!(
                " - prior work on {} at {} (optional)",
                thread.branch_name,
                thread
                    .supervision
                    .pushed_commit
                    .as_deref()
                    .unwrap_or_default()
            ));
        }
    }
    Ok(WorkPool::new(project.id, pieces))
}

fn task_pieces(project: &ProjectItem) -> Vec<WorkPiece> {
    project
        .tasks
        .iter()
        .filter_map(|task| {
            if !matches!(task.state, TaskState::Todo | TaskState::Now) {
                return None;
            }
            let owner = task
                .assigned_thread_id
                .and_then(|identifier| {
                    project.threads.iter().find(|thread| {
                        !thread.supervision.released
                            && thread.id == identifier
                            && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
                    })
                })
                .map(|thread| thread.id);
            if task.state == TaskState::Now && owner.is_none() {
                return None;
            }
            Some(WorkPiece::new(
                WorkKey::Task {
                    identifier: task.id,
                },
                task.text.clone(),
                "Task List".into(),
                owner,
            ))
        })
        .collect()
}

fn available_spec_pieces(project: &ProjectItem, markdown: &str) -> Vec<WorkPiece> {
    spec_pieces(markdown)
        .into_iter()
        .filter_map(|mut piece| {
            let WorkKey::Spec { section } = &piece.key else {
                return None;
            };
            if project.built_spec_sections.contains(section) {
                return None;
            }
            let owners: Vec<_> = project
                .threads
                .iter()
                .filter(|thread| {
                    !thread.supervision.released && thread.spec_section.as_ref() == Some(section)
                })
                .collect();
            if owners
                .iter()
                .any(|thread| thread.state == ThreadState::Merged)
            {
                return None;
            }
            piece.thread_identifier = owners.first().map(|thread| thread.id);
            Some(piece)
        })
        .collect()
}
