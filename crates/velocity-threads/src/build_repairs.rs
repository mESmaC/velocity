//! Plan a narrowly scoped, ordinary build thread for an unreverted culprit (#406).

use velocity_rosen::build_repairs::BuildFailure;
use velocity_store::build_repairs::BuildRepair;
use velocity_store::{ProjectItem, StateStore, ThreadState};

use crate::launch::{LaunchPlan, plan_named_launch};
use crate::workspace_retry::{WorkspaceRetry, plan_retry};

/// Returns the same pending launch after a crash, and never replaces an existing thread.
pub fn prepare_repair(
    store: &mut StateStore,
    project: u32,
    failure: &BuildFailure,
) -> Result<Option<(LaunchPlan, u32)>, String> {
    let current = store.project(project).ok_or("Unknown project")?;
    if let Some(repair) = current
        .build_repairs
        .iter()
        .find(|repair| repair.commit == failure.commit)
    {
        return restore_repair(store, current, repair);
    }
    if !current.spec_locked {
        return Err("Lock the spec before starting a build repair".into());
    }
    let preferences = store.worktree_preferences(current);
    let prompt = repair_prompt(failure);
    let title = format!(
        "Fix build after {}",
        &failure.commit[..failure.commit.len().min(12)]
    );
    let slug = format!(
        "fix-build-{}",
        &failure.commit[..failure.commit.len().min(12)]
    );
    let mut plan = plan_named_launch(
        current,
        &preferences,
        &store.settings(),
        &prompt,
        &title,
        &slug,
    )
    .map_err(|error| error.to_string())?;
    plan.title.request = None;
    let thread = store
        .add_build_repair(project, &failure.commit, plan.thread.clone(), &prompt)
        .map_err(|error| error.to_string())?;
    Ok(thread.map(|thread| (plan, thread)))
}

fn restore_repair(
    store: &StateStore,
    project: &ProjectItem,
    repair: &BuildRepair,
) -> Result<Option<(LaunchPlan, u32)>, String> {
    if repair.closed {
        return Ok(None);
    }
    let Some(thread) = project
        .threads
        .iter()
        .find(|thread| thread.id == repair.thread)
    else {
        return Ok(None);
    };
    if thread.pending_rosen_launch.is_none()
        || !matches!(thread.state, ThreadState::Working | ThreadState::Error)
    {
        return Ok(None);
    }
    let preferences = store.worktree_preferences(project);
    match plan_retry(project, thread, &preferences, &store.settings())
        .map_err(|error| error.to_string())?
    {
        WorkspaceRetry::Thread(plan) => Ok(Some((*plan, thread.id))),
        WorkspaceRetry::Goal(_) => Err("A build repair cannot be a goal swarm member".into()),
    }
}

pub fn repair_prompt(failure: &BuildFailure) -> String {
    let reason = failure
        .refusal
        .as_deref()
        .unwrap_or("The build culprit has not been reverted within two minutes of being recorded.");
    let blockers = if failure.blocking_commits.is_empty() {
        "Rosen reported no overlapping later commits.".into()
    } else {
        failure.blocking_commits.join("\n")
    };
    format!(
        "Make the project's base build again and touch nothing else. Fix this confirmed \
         build failure forward in your Rosen workspace. If the repair needs a spec decision, \
         find its heading with graph_find and read it with graph_read (source: true). \
         Use the normal build-thread lifecycle: claim the files, verify the build and lint \
         through Rosen's queue, and name the finished change with rosen commit -m. Velocity \
         lands it. Keep the repair limited to restoring the build; do not take other work.\n\n\
         Culprit change: {}\nCulprit commit: {}\n\nRevert status: {reason}\n\n\
         Later commits blocking the revert:\n{blockers}\n\nFailing check output:\n{}",
        failure.change, failure.commit, failure.output,
    )
}
