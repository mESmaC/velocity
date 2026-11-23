//! Plan a narrowly scoped, ordinary build thread for an unreverted culprit (#406).

use velocity_core::repair_tests::{
    TargetedTest, format_required_test_commands, parse_cargo_test_failures,
    rosen_platform_matches_host,
};
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
    let required_tests =
        build_required_tests(failure, std::env::consts::OS, std::env::consts::ARCH);
    let prompt = repair_prompt_with_tests(failure, &required_tests);
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
        .add_build_repair_with_tests(
            project,
            &failure.commit,
            plan.thread.clone(),
            &prompt,
            required_tests,
        )
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
    repair_prompt_for_host(failure, std::env::consts::OS, std::env::consts::ARCH)
}

/// Like [`repair_prompt`], evaluating same-platform test execution against
/// `(host_os, host_arch)` (#741).
pub fn repair_prompt_for_host(failure: &BuildFailure, host_os: &str, host_arch: &str) -> String {
    let required = build_required_tests(failure, host_os, host_arch);
    repair_prompt_with_tests(failure, &required)
}

fn build_required_tests(
    failure: &BuildFailure,
    host_os: &str,
    host_arch: &str,
) -> Vec<TargetedTest> {
    if !rosen_platform_matches_host(failure.platform.as_deref(), host_os, host_arch) {
        return Vec::new();
    }
    parse_cargo_test_failures(&failure.output)
}

fn repair_prompt_with_tests(failure: &BuildFailure, required_tests: &[TargetedTest]) -> String {
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
    let guidance = if required_tests.is_empty() {
        "Make the project's base build again and touch nothing else. Fix this confirmed \
         build failure forward in your Rosen workspace. If the repair needs a spec decision, \
         find its heading with graph_find and read it with graph_read (source: true). \
         Claim the files, queue a type-check for the crate you change, and name the finished change with rosen commit -m. Velocity \
         lands it. Keep the repair limited to restoring the build; do not take other work."
            .to_owned()
    } else {
        format!(
            "Required test command(s) before `rosen commit` (queued through the run queue):\n{}\n\n\
             Make the project's base build again and touch nothing else. Fix this confirmed \
             test failure forward in your Rosen workspace. Never weaken, skip or delete the \
             failing test. If the repair needs a spec decision, find its heading with graph_find \
             and read it with graph_read (source: true). Claim the files, fix the fault, and run \
             every required test command listed above through the run queue (`queue_run` with \
             `program: \"cargo\"` and the `cargo` arguments, or the `rosen run -- ...` command \
             when falling back to the shell). If any required test fails, keep working until it \
             passes. Velocity refuses `rosen commit -m` until those tests and `preflight` pass on \
             the current snapshot. Velocity lands the named change. Keep the repair limited to \
             restoring the build; do not take other work.",
            format_required_test_commands(required_tests)
        )
    };
    let instructions = format!(
        "Culprit change: {}\nCulprit commit: {}\nRevert status: {}\n\n\
         Later commits blocking the revert:\n{blockers}\n\n\
         {guidance}",
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

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#741): a local integration test failure repair brief must include the exact
    // targeted `cargo test` command so the repair thread verifies the failing test passes
    // before naming its change instead of landing a type-check-only fix that leaves base red.
    #[test]
    fn integration_test_failure_brief_includes_targeted_test_command() {
        let output = "Integration build failed\n\
running 15 tests\n\
test dependencies::tests::refresh_wakes_reserved_slug_build_holders_and_skips_non_thread_holders ... FAILED\n\n\
failures:\n\n\
---- dependencies::tests::refresh_wakes_reserved_slug_build_holders_and_skips_non_thread_holders stdout ----\n\
Error: Invalid(\"Claim your piece before waiting\")\n\n\
failures:\n\
    dependencies::tests::refresh_wakes_reserved_slug_build_holders_and_skips_non_thread_holders\n\n\
test result: FAILED. 14 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s\n\n\
error: test failed, to rerun pass `-p velocity-threads --lib`\n";
        let failure = BuildFailure::new(
            "456b01c10f7c0000000000000000000000000000".into(),
            "opmwpnyv".into(),
            output.into(),
            0,
        )
        .with_refusal("Integration failures are fixed forward.".into(), Vec::new());

        let prompt = repair_prompt_for_host(&failure, "windows", "x86_64");
        assert!(prompt.contains(
            "rosen run -- cargo test -p velocity-threads --lib dependencies::tests::refresh_wakes_reserved_slug_build_holders_and_skips_non_thread_holders"
        ));
        assert!(prompt.contains("Velocity refuses `rosen commit -m` until those tests"));
        assert!(!prompt.contains("queue a type-check for the crate you change"));
    }
}
