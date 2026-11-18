//! Restore a failed Rosen launch from saved state without creating a new thread (#407).

use velocity_core::text_budget::{SHORT_PROMPT_BYTES, prompt_excerpt};
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_store::goal_swarms::GoalAgentSlot;
use velocity_store::{ProjectItem, Settings, ThreadItem};

use crate::goal_swarm::{GoalMemberPlan, plan_goal_member};
use crate::launch::{LaunchError, LaunchPlan, plan_launch_with_preferences};

pub enum WorkspaceRetry {
    Thread(Box<LaunchPlan>),
    Goal(Box<GoalMemberPlan>),
}

/// Uses the original prompt, provider and folder, even if project defaults have changed.
pub fn plan_retry(
    project: &ProjectItem,
    thread: &ThreadItem,
    preferences: &WorktreePreferences,
    settings: &Settings,
) -> Result<WorkspaceRetry, LaunchError> {
    let mut settings = settings.clone();
    settings.providers.default_provider = thread.provider;
    settings.providers.default_model = Some(thread.model_name.clone());
    settings.providers.default_effort = thread.effort;
    settings.providers.default_speed = thread.preferred_speed;
    settings.ensure_new_work(thread.provider)?;
    // Naming must not treat this thread's own saved folder and branch as taken.
    let mut project = project.clone();
    project.threads.retain(|item| item.id != thread.id);
    let path = thread.worktree_path.as_deref().unwrap_or_default();
    if let Some(slot) = GoalAgentSlot::for_thread(&project, thread) {
        let mut plan = plan_goal_member(&project, preferences, &settings, &slot)?;
        plan.worktree.worktree_path = path.into();
        plan.worktree.branch_name = Some(thread.branch_name.clone());
        plan.thread.worktree_path = path.into();
        return Ok(WorkspaceRetry::Goal(Box::new(plan)));
    }
    let prompt = thread.pending_rosen_launch.as_deref().unwrap_or_default();
    let mut plan = plan_launch_with_preferences(&project, preferences, &settings, prompt)?;
    if prompt.len() > SHORT_PROMPT_BYTES
        && let Some(hint) = dispatched_prompt_hint(thread)
    {
        plan.prompt = prompt_excerpt(prompt, SHORT_PROMPT_BYTES, &hint);
    }
    plan.worktree.worktree_path = path.into();
    plan.worktree.branch_name = Some(thread.branch_name.clone());
    plan.thread.worktree_path = path.into();
    plan.thread.branch_name.clone_from(&thread.branch_name);
    plan.title.request = None;
    Ok(WorkspaceRetry::Thread(Box::new(plan)))
}

fn dispatched_prompt_hint(thread: &ThreadItem) -> Option<String> {
    if let Some(issue) = &thread.issue {
        return Some(format!(
            "\n[Issue excerpt. Open https://github.com/{}/issues/{} for the full description and evidence. Follow the Rosen workspace note.]",
            issue.repository, issue.number
        ));
    }
    if let Some(task) = thread.task_ids.first() {
        return Some(format!(
            "\n[Task excerpt. Read the complete task with task_list using task {task}. Follow the Rosen workspace note.]"
        ));
    }
    None
}
