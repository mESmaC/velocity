//! Single-issue launch planning shared by the native adapter and headless driver (F9).
use thiserror::Error;
use velocity_github::{GitHubIssue, IssueState, SpecQuestion};
use velocity_store::StateStore;
use velocity_store::build_gate::{LaunchBlocked, LaunchRequest, require_launch};
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::threads::ThreadError;

use crate::{LaunchError, LaunchPlan};

pub fn associate_issue_threads(
    project: &velocity_store::ProjectItem,
    repository: &str,
    rows: &mut [velocity_github::IssuePanelRow],
) {
    for row in rows {
        let assignment = ThreadIssue::new(repository.into(), row.number);
        let thread = issue_thread(project, &assignment);
        row.thread_identifier = thread.map(|thread| thread.id);
        row.thread_label = thread.map(velocity_store::issues::issue_thread_label);
    }
}

pub fn issue_palette_rows(
    rows: &[velocity_github::IssuePanelRow],
) -> Vec<velocity_core::layout_view::RailItem> {
    rows.iter()
        .filter(|row| row.question.is_none())
        .map(|row| {
            velocity_core::layout_view::RailItem::new(
                row.number as usize,
                row.title.clone(),
                row.labels.join(" · "),
            )
        })
        .collect()
}

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error(transparent)]
    Gate(#[from] LaunchBlocked),
    #[error(transparent)]
    Launch(#[from] LaunchError),
    #[error(transparent)]
    Store(#[from] ThreadError),
    #[error("{0}")]
    Invalid(String),
}

pub struct IssueDispatchPlan {
    pub launch: LaunchPlan,
    pub issue: ThreadIssue,
}

impl IssueDispatchPlan {
    pub fn record(&self, store: &mut StateStore) -> Result<u32, DispatchError> {
        require_launch(store.state(), LaunchRequest::IssueDispatch)?;
        if store.active_project().map(|project| project.id) != Some(self.launch.project) {
            return Err(DispatchError::Invalid(
                "The selected project changed; dispatch again".into(),
            ));
        }
        Ok(store.add_issue_thread(
            self.launch.project,
            self.launch.thread.clone(),
            self.issue.clone(),
        )?)
    }
}

pub fn plan_issue_dispatch(
    store: &StateStore,
    project_identifier: u32,
    repository: &str,
    issue: &GitHubIssue,
    accept_recommendation: bool,
) -> Result<IssueDispatchPlan, DispatchError> {
    require_launch(store.state(), LaunchRequest::IssueDispatch)?;
    let _project = store
        .active_project()
        .filter(|project| project.id == project_identifier)
        .ok_or_else(|| {
            DispatchError::Invalid("The selected project changed; dispatch again".into())
        })?;
    plan_project_issue_dispatch(
        store,
        project_identifier,
        repository,
        issue,
        accept_recommendation,
    )
}

/// Background dispatch uses the named project, even while another project is selected.
pub fn plan_project_issue_dispatch(
    store: &StateStore,
    project_identifier: u32,
    repository: &str,
    issue: &GitHubIssue,
    accept_recommendation: bool,
) -> Result<IssueDispatchPlan, DispatchError> {
    let project = store
        .project(project_identifier)
        .ok_or_else(|| DispatchError::Invalid("The project was removed".into()))?;
    if !project.spec_locked {
        return Err(LaunchBlocked::LockSpecFirst.into());
    }
    if issue.state != IssueState::Open {
        return Err(DispatchError::Invalid(format!(
            "Issue #{} is closed",
            issue.number
        )));
    }
    let assignment = ThreadIssue::new(repository.into(), issue.number);
    if let Some(thread) = issue_thread(project, &assignment) {
        return Err(ThreadError::IssueAssigned {
            number: issue.number,
            thread: thread.id,
        }
        .into());
    }
    let prompt = dispatch_prompt(
        repository,
        issue,
        accept_recommendation,
        project.version_control,
    )?;
    let mut launch = crate::launch::plan_named_launch(
        project,
        &store.worktree_preferences(project),
        &store.settings(),
        &prompt,
        &issue.title,
        &format!("issue-{}", issue.number),
    )?;
    // F4: the issue already supplies its title, including recommendation threads.
    launch.title.request = None;
    Ok(IssueDispatchPlan {
        launch,
        issue: assignment,
    })
}

fn dispatch_prompt(
    repository: &str,
    issue: &GitHubIssue,
    accept: bool,
    version_control: velocity_store::VersionControl,
) -> Result<String, DispatchError> {
    let scope = if accept {
        let question = SpecQuestion::from_body(&issue.body);
        if !issue
            .labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case("spec-question"))
            || question.recommendation.is_empty()
        {
            return Err(DispatchError::Invalid(
                "This question has no recommended answer to accept".into(),
            ));
        }
        let delivery = if version_control == velocity_store::VersionControl::Rosen {
            "Name the Rosen change after making that one-line edit."
        } else {
            "Route the one-line PR through normal PR duty review as Trivial."
        };
        format!(
            "The user accepts this recommended answer: {}\nRecord the answer as *(proposed)* in the relevant §8 question. Change only that decision line. {delivery}",
            question.recommendation
        )
    } else {
        "Implement this issue end to end, following AGENTS.md and SPEC.md. Read its evidence before editing.".into()
    };
    if version_control == velocity_store::VersionControl::Rosen {
        return Ok(rosen_dispatch_prompt(&scope, repository, issue));
    }
    Ok(format!(
        "{scope}\n\nIssue {repository}#{}: {}\n{}\n\n{}\n\nUse issue_comment for starting work, findings that change the plan, and blockers.\n\nYou already have your own worktree and branch. Open a PR against the project's default branch with Fixes #{} in its description. Watch it until it merges, fix failed checks, answer every review comment, resolve addressed threads and re-request review. Never merge your own PR. Messages received while busy are queued.",
        issue.number, issue.title, issue.url, issue.body, issue.number
    ))
}

fn rosen_dispatch_prompt(scope: &str, repository: &str, issue: &GitHubIssue) -> String {
    format!(
        "{scope}\n\nIssue {repository}#{}: {}\n{}\n\n{}\n\nFollow the Rosen workspace note Velocity gave you. If there is no work to change, explain why and end your turn.",
        issue.number, issue.title, issue.url, issue.body
    )
}
