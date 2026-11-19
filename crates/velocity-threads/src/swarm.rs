//! Planning swarm members (SPEC.md F10): one ordinary thread per item, each in its own
//! worktree, with the item's title and no title model call (F4).

use velocity_core::thread_titles::title_slug;
use velocity_store::StateStore;
use velocity_store::build_gate::{LaunchRequest, require_launch};
use velocity_store::issues::ThreadIssue;
use velocity_store::swarm_dialog::provider_model_name;
use velocity_store::swarms::{LaunchSwarmRequest, SwarmItem};
use velocity_store::{ProjectItem, ProviderKind, TaskState};

use crate::LaunchPlan;
use crate::dispatch::DispatchError;

/// One member ready to record and start.
pub struct SwarmMemberPlan {
    pub launch: LaunchPlan,
    pub item: SwarmItem,
    pub issue: Option<ThreadIssue>,
}

/// An open issue as the Issues panel last read it.
pub struct OpenIssue {
    pub number: u32,
    pub title: String,
    pub assigned: bool,
}

/// The build gate for this swarm's source (Rule 2): checked again before anything runs.
pub fn require_swarm_launch(
    store: &StateStore,
    request: &LaunchSwarmRequest,
) -> Result<(), DispatchError> {
    let kind = if request
        .items
        .iter()
        .all(|item| matches!(item, SwarmItem::Task(_)))
    {
        LaunchRequest::TaskSwarm
    } else {
        LaunchRequest::IssueSwarm
    };
    require_launch(store.state(), kind)?;
    if store.active_project().map(|project| project.id) != Some(request.project_identifier) {
        return Err(DispatchError::Invalid(
            "The selected project changed; open the swarm dialog again".into(),
        ));
    }
    if request.items.is_empty() {
        return Err(DispatchError::Invalid("Select at least one item".into()));
    }
    Ok(())
}

/// Plans the member for `item` on `provider`. Items that gained an agent since the dialog
/// was drawn, closed issues and draft tasks are refused.
pub fn plan_swarm_member(
    store: &StateStore,
    request: &LaunchSwarmRequest,
    repository: &str,
    issues: &[OpenIssue],
    item: SwarmItem,
    provider: ProviderKind,
) -> Result<SwarmMemberPlan, DispatchError> {
    let project = store
        .active_project()
        .filter(|project| project.id == request.project_identifier)
        .ok_or_else(|| DispatchError::Invalid("The selected project changed".into()))?;
    let (title, slug, prompt, issue) = member_work(project, request, repository, issues, item)?;
    // The member runs on its round-robin provider; everything else is the user's settings.
    // A provider you signed out of since the dialog was drawn is refused, never swapped.
    let mut settings = store.settings();
    settings
        .ensure_new_work(provider)
        .map_err(crate::launch::LaunchError::from)?;
    settings.providers.default_model = Some(provider_model_name(&settings, provider));
    settings.providers.default_provider = provider;
    let mut launch = crate::launch::plan_named_launch(
        project,
        &store.worktree_context().preferences,
        &settings,
        &prompt,
        &title,
        &slug,
    )?;
    launch.title.request = None;
    Ok(SwarmMemberPlan {
        launch,
        item,
        issue,
    })
}

type MemberWork = (String, String, String, Option<ThreadIssue>);

/// The member's title, branch slug, opening prompt and linked issue, when the item is
/// still open and has no agent.
fn member_work(
    project: &ProjectItem,
    request: &LaunchSwarmRequest,
    repository: &str,
    issues: &[OpenIssue],
    item: SwarmItem,
) -> Result<MemberWork, DispatchError> {
    let work = match item {
        SwarmItem::Task(identifier) => {
            let task = project
                .tasks
                .iter()
                .find(|task| task.id == identifier)
                .filter(|task| task.state == TaskState::Todo && task.assigned_thread_id.is_none())
                .ok_or_else(|| {
                    DispatchError::Invalid(format!("Task {identifier} is no longer open"))
                })?;
            let prompt = task_prompt(task.id, &task.text, project.version_control);
            (task.text.clone(), title_slug(&task.text), prompt, None)
        }
        SwarmItem::Issue(number) => {
            let issue = issues
                .iter()
                .find(|issue| issue.number == number && !issue.assigned)
                .ok_or_else(|| {
                    DispatchError::Invalid(format!("Issue #{number} is closed or has an agent"))
                })?;
            let prompt = issue_prompt(
                repository,
                issue,
                request.close_issues,
                project.version_control,
            );
            let owner = ThreadIssue::new(repository.into(), number);
            (
                issue.title.clone(),
                format!("issue-{number}"),
                prompt,
                Some(owner),
            )
        }
    };
    Ok(work)
}

const LIFECYCLE: &str = "You already have your own worktree and branch. Open a PR against the project's default branch and watch it until it merges: fix failed checks, answer every review comment, resolve addressed threads and re-request review. Never merge your own PR. Messages from the swarm arrive between your steps.";

pub(crate) fn task_excerpt(identifier: u32, text: &str) -> String {
    velocity_core::text_budget::prompt_excerpt(
        text,
        2_600,
        &format!("\n[Read the complete task with task_list using task {identifier}.]"),
    )
}

fn task_prompt(
    identifier: u32,
    task: &str,
    version_control: velocity_store::VersionControl,
) -> String {
    let delivery = if version_control == velocity_store::VersionControl::Rosen {
        "Follow the Rosen workspace note Velocity gave you. Name the task in the snapshot."
            .to_owned()
    } else {
        format!("Name the task in your PR description.\n\n{LIFECYCLE}")
    };
    format!(
        "Your swarm item is Task List item #{identifier}:\n\n{}\n\nBuild it end to end. {}\n\n{delivery}",
        task_excerpt(identifier, task),
        crate::task_spec::instructions(&[task])
    )
}

fn issue_prompt(
    repository: &str,
    issue: &OpenIssue,
    close: bool,
    version_control: velocity_store::VersionControl,
) -> String {
    let scope = format!(
        "Your swarm item is issue {repository}#{}: {}. Read its description and evidence, then implement it end to end. {}",
        issue.number,
        issue.title,
        crate::task_spec::instructions(&[&issue.title])
    );
    if version_control == velocity_store::VersionControl::Rosen {
        return format!(
            "{scope}\n\nhttps://github.com/{repository}/issues/{}\n\nFollow the Rosen workspace note Velocity gave you.",
            issue.number
        );
    }
    let link = if close {
        format!("Write Fixes #{} in the PR description.", issue.number)
    } else {
        format!(
            "Write Refs #{} in the PR description; leave the issue open.",
            issue.number
        )
    };
    format!(
        "{scope}\n\nRead it with `gh issue view {}`. Use issue_comment for starting work, findings that change the plan, and blockers. {link}\n\n{LIFECYCLE}",
        issue.number
    )
}
