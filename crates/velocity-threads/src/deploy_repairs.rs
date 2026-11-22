//! Plan a narrowly scoped fix-forward thread for a failed `[deploy.<step>]` run (#594).
//!
//! Secret values are redacted before `DeployFailure` is constructed; the repair agent
//! never receives deploy secrets in its environment, prompt, or transcript.

use velocity_core::text_budget::{SHORT_PROMPT_BYTES, preview};
use velocity_store::deploy_repairs::DeployRepair;
use velocity_store::{ProjectItem, StateStore, ThreadState};

use crate::build_repairs::failure_excerpt;
use crate::launch::{LaunchPlan, plan_named_launch};
use crate::workspace_retry::{WorkspaceRetry, plan_retry};

const INSTRUCTIONS: &str = "Fix this failed deploy forward in your Rosen workspace and touch \
nothing else. Secret values have been redacted from the output above and are held only in the \
owner's local credential store (never commit secrets or ask to print them). Check first that the \
base still has the fault: if a later change already fixed it, say so and end your turn. If the \
repair needs a spec decision, find its heading with graph_find and read it with graph_read \
(source: true). Claim the files, run preflight, and name the finished change with rosen commit \
-m. Velocity lands it, and the next passing deploy of this step closes the failure. Keep the \
repair limited to fixing this deploy; do not take other work.";

/// Confirmed failure of one `[deploy.<step>]` command with redacted output.
#[derive(Clone, Debug)]
pub struct DeployFailure {
    pub run_id: i64,
    pub step: String,
    pub commit: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub stopped_reason: Option<String>,
    /// Output already redacted by `velocity_core::deploy::redact_secret_pairs`.
    pub redacted_output: String,
}

/// Allocates a fix-forward thread and `DeployRepair` record, or returns an existing
/// pending launch after a crash. Never starts a duplicate thread for the same run.
pub fn prepare_deploy_repair(
    store: &mut StateStore,
    project: u32,
    failure: &DeployFailure,
) -> Result<Option<(LaunchPlan, u32)>, String> {
    let current = store.project(project).ok_or("Unknown project")?;
    if let Some(repair) = current.deploy_repairs.iter().find(|repair| {
        repair.run_id == failure.run_id
            || (!repair.closed && repair.step == failure.step && repair.commit == failure.commit)
    }) {
        return restore_repair(store, current, repair, failure);
    }
    if !current.spec_locked {
        return Err("Lock the spec before starting a deploy repair".into());
    }
    let preferences = store.worktree_preferences(current);
    let prompt = deploy_repair_prompt(failure);
    let short = failure.commit.get(..12).unwrap_or(&failure.commit);
    let title = preview(&format!("Fix deploy: {} ({short})", failure.step), 80);
    let slug = format!("fix-deploy-{}-{short}", sanitize_slug(&failure.step));
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
        .add_deploy_repair(
            project,
            failure.run_id,
            &failure.step,
            &failure.commit,
            plan.thread.clone(),
            &prompt,
        )
        .map_err(|error| error.to_string())?;
    Ok(thread.map(|thread| (plan, thread)))
}

/// Returns launches for saved deploy repairs whose threads never finished starting.
pub fn pending_deploy_repairs(store: &StateStore, project: u32) -> Vec<(LaunchPlan, u32)> {
    let Some(current) = store.project(project) else {
        return Vec::new();
    };
    let preferences = store.worktree_preferences(current);
    let settings = store.settings();
    current
        .deploy_repairs
        .iter()
        .filter(|repair| !repair.closed)
        .filter_map(|repair| {
            let thread = current
                .threads
                .iter()
                .find(|thread| thread.id == repair.thread)?;
            if thread.pending_rosen_launch.is_none()
                || !matches!(thread.state, ThreadState::Working | ThreadState::Error)
            {
                return None;
            }
            match plan_retry(current, thread, &preferences, &settings).ok()? {
                WorkspaceRetry::Thread(plan) => Some((*plan, repair.thread)),
                WorkspaceRetry::Goal(_) => None,
            }
        })
        .collect()
}

/// Builds the budgeted opening prompt for a deploy fix-forward thread.
pub fn deploy_repair_prompt(failure: &DeployFailure) -> String {
    let status_line = match (&failure.stopped_reason, failure.exit_code) {
        (Some(reason), _) => format!("Result: {reason}"),
        (None, Some(code)) => format!("Exit code: {code}"),
        (None, None) => "Result: failed".into(),
    };
    let header = format!(
        "A post-land `[deploy]` step failed after a green land.\n\
         Step: {}\n\
         Green commit: {}\n\
         Command: {}\n\
         {status_line}\n\n\
         Failing deploy output (secrets redacted):\n",
        preview(&failure.step, 80),
        preview(&failure.commit, 64),
        preview(&failure.command, 200),
    );
    let available = SHORT_PROMPT_BYTES.saturating_sub(header.len() + INSTRUCTIONS.len() + 4);
    let body = if failure.redacted_output.trim().is_empty() {
        "(no output)".to_owned()
    } else if failure.redacted_output.len() <= available {
        failure.redacted_output.clone()
    } else {
        failure_excerpt(&failure.redacted_output, available)
    };
    format!("{header}{body}\n\n{INSTRUCTIONS}")
}

fn restore_repair(
    store: &StateStore,
    project: &ProjectItem,
    repair: &DeployRepair,
    failure: &DeployFailure,
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
    match plan_retry(project, thread, &preferences, &settings_snapshot(store))
        .map_err(|error| error.to_string())?
    {
        WorkspaceRetry::Thread(mut plan) => {
            if plan.prompt.len() > SHORT_PROMPT_BYTES {
                plan.prompt = deploy_repair_prompt(failure);
            }
            Ok(Some((*plan, thread.id)))
        }
        WorkspaceRetry::Goal(_) => Err("A deploy repair cannot be a goal swarm member".into()),
    }
}

fn settings_snapshot(store: &StateStore) -> velocity_store::Settings {
    store.settings()
}

fn sanitize_slug(step: &str) -> String {
    let cleaned: String = step
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "step".into()
    } else {
        trimmed.chars().take(24).collect()
    }
}
