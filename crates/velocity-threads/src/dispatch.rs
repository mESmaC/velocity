//! Single-issue launch planning shared by the native adapter and headless driver (F9).
use thiserror::Error;
use velocity_core::text_budget::{SHORT_PROMPT_BYTES, preview};
use velocity_github::{GitHubIssue, IssueState, SpecQuestion};
use velocity_store::StateStore;
use velocity_store::build_gate::{LaunchBlocked, LaunchRequest, require_launch};
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::threads::ThreadError;

use crate::{LaunchError, LaunchPlan};

/// Per-issue thread and hold association extracted under the store lock so `issue_panel`
/// can drop the store lock before acquiring panel/layout locks or emitting events (#554).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueRowLink {
    pub number: u32,
    pub thread_identifier: Option<u32>,
    pub thread_label: Option<String>,
    pub hold: Option<velocity_github::IssueHold>,
}

pub fn collect_issue_row_links(
    project: &velocity_store::ProjectItem,
    repository: &str,
    numbers: &[u32],
) -> Vec<IssueRowLink> {
    let now = now_unix_seconds();
    numbers
        .iter()
        .copied()
        .map(|number| {
            let assignment = ThreadIssue::new(repository.into(), number);
            let thread = issue_thread(project, &assignment);
            let thread_identifier = thread.map(|thread| thread.id);
            let mut thread_label = thread.map(velocity_store::issues::issue_thread_label);
            // A landed issue stays open until a green integration build closes it (#504).
            if thread.is_none()
                && project
                    .landed_awaiting_integration(&assignment, None)
                    .is_some()
            {
                thread_label = Some("landed · waiting for integration".into());
            } else if thread.is_none() && project.remote_work_hold(&assignment, None, now).is_some()
            {
                thread_label = Some("in progress elsewhere".into());
            }
            // The card says why dispatch has not started the issue and which files overlap (#525).
            let hold = project.overlap_hold(&assignment).map(|wait| {
                velocity_github::IssueHold::new(wait.label(), wait.reason(), wait.paths.clone())
            });
            IssueRowLink {
                number,
                thread_identifier,
                thread_label,
                hold,
            }
        })
        .collect()
}

pub fn apply_issue_row_links(
    rows: &mut [velocity_github::IssuePanelRow],
    links: &[IssueRowLink],
) -> bool {
    let mut changed = false;
    for row in rows {
        let Some(link) = links.iter().find(|link| link.number == row.number) else {
            continue;
        };
        if row.thread_identifier != link.thread_identifier {
            row.thread_identifier = link.thread_identifier;
            changed = true;
        }
        if row.thread_label != link.thread_label {
            row.thread_label = link.thread_label.clone();
            changed = true;
        }
        if row.hold != link.hold {
            row.hold = link.hold.clone();
            changed = true;
        }
    }
    changed
}

pub fn associate_issue_threads(
    project: &velocity_store::ProjectItem,
    repository: &str,
    rows: &mut [velocity_github::IssuePanelRow],
) {
    let numbers: Vec<u32> = rows.iter().map(|row| row.number).collect();
    let links = collect_issue_row_links(project, repository, &numbers);
    let _ = apply_issue_row_links(rows, &links);
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
    pub updated_at: String,
    pub reopened_at: Option<String>,
}

impl IssueDispatchPlan {
    pub fn record(&self, store: &mut StateStore) -> Result<u32, DispatchError> {
        require_launch(store.state(), LaunchRequest::IssueDispatch)?;
        if store.active_project().map(|project| project.id) != Some(self.launch.project) {
            return Err(DispatchError::Invalid(
                "The selected project changed; dispatch again".into(),
            ));
        }
        let version = velocity_store::rosen_work::IssueVersion {
            updated_at: Some(self.updated_at.as_str()),
            reopened_at: self.reopened_at.as_deref(),
            title: self.issue.title.as_deref(),
            body: None,
        };
        Ok(store.add_issue_thread_for_version(
            self.launch.project,
            self.launch.thread.clone(),
            self.issue.clone(),
            version,
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
    let assignment = ThreadIssue::new(repository.into(), issue.number)
        .with_identity(issue.title.clone(), issue.host);
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
        updated_at: issue.updated_at.clone(),
        reopened_at: issue.reopened_at.clone(),
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
            "Open a small PR with the spec decision for the repository maintainers."
        };
        let recommendation = velocity_core::text_budget::prompt_excerpt(
            &question.recommendation,
            800,
            "\n[Read the full recommendation in the linked issue before recording it.]",
        );
        format!(
            "The user accepts this recommended answer: {}\nRecord the answer as *(proposed)* in the relevant §8 question. Change only that decision line. {} {delivery}",
            recommendation,
            crate::task_spec::named_section("§8 Open questions")
        )
    } else {
        format!(
            "Read the issue evidence before editing and implement the issue end to end. {}",
            crate::task_spec::instructions(&[&issue.title, &issue.body])
        )
    };
    Ok(issue_brief(&scope, repository, issue, version_control))
}

fn issue_brief(
    scope: &str,
    repository: &str,
    issue: &GitHubIssue,
    version_control: velocity_store::VersionControl,
) -> String {
    let delivery = if version_control == velocity_store::VersionControl::Rosen {
        "Read the issue discussion for evidence and any instructions for retrying after an empty turn. Follow the Rosen workspace note Velocity gave you. If there is no work to change, explain why and end your turn.".to_owned()
    } else {
        format!(
            "Use issue_comment for starting work, findings that change the plan, and blockers.\n\nYou already have your own worktree and branch. Open a PR against the project's default branch with Fixes #{} in its description. Watch it until it merges, fix failed checks, answer every review comment, resolve addressed threads and re-request review. Never merge your own PR. Messages received while busy are queued.",
            issue.number
        )
    };
    let title = preview(&issue.title, 320);
    let header = format!(
        "Issue {repository}#{}: {title}\n{}\n\n",
        issue.number, issue.url
    );
    let instructions = format!("{scope}\n\n{delivery}");
    let notice = "\n[Issue excerpt. Open the linked issue for the full description and evidence.]";
    let available = SHORT_PROMPT_BYTES.saturating_sub(header.len() + instructions.len() + 2);
    if issue.body.len() <= available {
        return format!("{header}{}\n\n{instructions}", issue.body);
    }
    let excerpt = preview(&issue.body, available.saturating_sub(notice.len()));
    // Repository and URL come from GitHub, but also bound the assembled prompt for old saved data.
    preview(
        &format!("{header}{excerpt}{notice}\n\n{instructions}"),
        SHORT_PROMPT_BYTES,
    )
}

fn now_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
