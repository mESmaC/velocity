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
    let mut settings = store.settings();
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
            let prompt = task_prompt(&task.text);
            (task.text.clone(), title_slug(&task.text), prompt, None)
        }
        SwarmItem::Issue(number) => {
            let issue = issues
                .iter()
                .find(|issue| issue.number == number && !issue.assigned)
                .ok_or_else(|| {
                    DispatchError::Invalid(format!("Issue #{number} is closed or has an agent"))
                })?;
            let prompt = issue_prompt(repository, issue, request.close_issues);
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

const LIFECYCLE: &str = "You already have your own worktree and branch. Open a PR against the project's default branch and watch it until it merges: fix failed checks, answer every review comment, resolve addressed threads and re-request review. Never merge your own PR. Messages from the swarm arrive between your steps. When one says a merged PR touched your files, rebase onto the default branch, resolve the conflicts, re-run the checks and push.";

fn task_prompt(task: &str) -> String {
    format!(
        "You are one agent in a swarm: one agent per open item, all working at once, each in its own worktree. Your item is this Task List task:\n\n{task}\n\nBuild it end to end, following AGENTS.md and SPEC.md. Name the task in your PR description.\n\n{LIFECYCLE}"
    )
}

fn issue_prompt(repository: &str, issue: &OpenIssue, close: bool) -> String {
    let link = if close {
        format!("Write Fixes #{} in the PR description.", issue.number)
    } else {
        format!(
            "Write Refs #{} in the PR description; leave the issue open.",
            issue.number
        )
    };
    format!(
        "You are one agent in a swarm: one agent per open item, all working at once, each in its own worktree. Your item is issue {repository}#{}: {}\n\nRead it first with `gh issue view {}`, including its evidence. Implement it end to end, following AGENTS.md and SPEC.md. {link}\n\n{LIFECYCLE}",
        issue.number, issue.title, issue.number
    )
}
