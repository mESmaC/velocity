//! Plan a narrowly scoped, ordinary build thread for an unreverted culprit (#406).

use velocity_core::text_budget::{SHORT_PROMPT_BYTES, prefix, preview};
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
        return restore_repair(store, current, repair, failure);
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
    plan.thread.role = velocity_core::tool_policy::ThreadRole::FollowUp;
    let thread = store
        .add_build_repair(project, &failure.commit, plan.thread.clone(), &prompt)
        .map_err(|error| error.to_string())?;
    Ok(thread.map(|thread| (plan, thread)))
}

fn restore_repair(
    store: &StateStore,
    project: &ProjectItem,
    repair: &BuildRepair,
    failure: &BuildFailure,
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
        WorkspaceRetry::Thread(mut plan) => {
            // Pending launches saved before the prompt budget may still hold the full log.
            if plan.prompt.len() > SHORT_PROMPT_BYTES {
                plan.prompt = repair_prompt(failure);
            }
            Ok(Some((*plan, thread.id)))
        }
        WorkspaceRetry::Goal(_) => Err("A build repair cannot be a goal swarm member".into()),
    }
}

pub fn repair_prompt(failure: &BuildFailure) -> String {
    let integrated = failure
        .output
        .starts_with(velocity_rosen::integration::FAILURE_HEADING);
    let reason = failure
        .refusal
        .as_deref()
        .unwrap_or("The build culprit has not been reverted within two minutes of being recorded.");
    let blockers = if integrated {
        "Integration keeps the landed history and uses a follow-up change.".into()
    } else if failure.blocking_commits.is_empty() {
        "Rosen reported no overlapping later commits.".into()
    } else {
        preview(&failure.blocking_commits.join("\n"), 500)
    };
    let instructions = format!(
        "Culprit change: {}\nCulprit commit: {}\nRevert status: {}\n\n\
         Later commits blocking the revert:\n{blockers}\n\n\
         Make the project's base build again and touch nothing else. Fix this confirmed \
         build failure forward in your Rosen workspace. If the repair needs a spec decision, \
         find its heading with graph_find and read it with graph_read (source: true). \
         Claim the files, queue a type-check for the crate you change, and name the finished change with rosen commit -m. Velocity \
         lands it. Keep the repair limited to restoring the build; do not take other work.",
        preview(&failure.change, 128),
        preview(&failure.commit, 128),
        preview(reason, 350),
    );
    let heading = "Failing check output:\n";
    let available = SHORT_PROMPT_BYTES - heading.len() - instructions.len() - 2;
    if failure.output.len() <= available {
        return format!("{heading}{}\n\n{instructions}", failure.output);
    }
    let notice = format!(
        "\n[Output excerpt. Read the full recorded output with `rosen review failures --change {} --json`.]",
        preview(&failure.change, 128)
    );
    let excerpt = failure_excerpt(&failure.output, available - notice.len());
    format!("{heading}{excerpt}{notice}\n\n{instructions}")
}

pub(crate) fn failure_excerpt(output: &str, budget: usize) -> String {
    let diagnostics = output
        .lines()
        .filter(|line| {
            let line = line.to_ascii_lowercase();
            [
                "error:",
                "error[",
                "fatal:",
                "failed",
                "failure",
                "panicked",
                "has been running for over 60 seconds",
                "exceeded the maximum execution time",
            ]
            .iter()
            .any(|term| line.contains(term))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if diagnostics.len() >= budget {
        return preview(&diagnostics, budget);
    }
    // Keep the final check summary and source context when diagnostics leave room.
    let tail_budget = budget.saturating_sub(diagnostics.len() + 2);
    let start = output.ceil_char_boundary(output.len().saturating_sub(tail_budget));
    let tail = &output[start..];
    if diagnostics.is_empty() {
        return if output.lines().count() <= 1 {
            preview(output, budget)
        } else {
            tail.to_owned()
        };
    }
    format!("{diagnostics}\n\n{}", prefix(tail, tail_budget))
}
