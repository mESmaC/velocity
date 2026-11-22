//! Plan the fix-forward thread for a GitHub CI failure on the base (F5 "Rosen · Base CI",
//! #507): an ordinary build thread whose prompt names the workflow, the failed jobs and
//! steps and an excerpt of their log.

use std::path::Path;

use velocity_core::base_ci::{self, FailedJob, Failure};
use velocity_core::text_budget::{SHORT_PROMPT_BYTES, preview};
use velocity_store::{StateStore, ThreadState};

use crate::build_repairs::failure_excerpt;
use crate::launch::{LaunchPlan, plan_named_launch};
use crate::workspace_retry::{WorkspaceRetry, plan_retry};

/// Jobs whose log is excerpted in the prompt. A matrix can fail in every cell for one
/// reason; the rest are named, and the saved log holds all of them.
const EXCERPTED_JOBS: usize = 3;

const INSTRUCTIONS: &str = "Make this workflow pass on the base again and touch nothing else. \
This is a confirmed failure on the landed base, like a failed integration build: fix it forward \
in your Rosen workspace. The job runs on GitHub's runner, which may be an operating system \
this machine cannot run, so work from the log and make the code correct on that platform. \
Check first that the base still has the fault: when another change already fixed it, say so and \
end your turn. Never weaken, skip or delete the failing test or lint. If the repair needs a spec \
decision, find its heading with graph_find and read it with graph_read (source: true). Claim the \
files, queue a type-check for the crate you change, and name the finished change with \
rosen commit -m. Velocity lands it, and the workflow's next green run on the base closes this \
failure. Keep the repair limited to this failure; do not take other work.";

const ROSEN_INSTRUCTIONS: &str = "Make this Rosen post-land check pass on the base again and \
touch nothing else. This is a confirmed per-platform failure on the landed base: fix it forward \
in your Rosen workspace. The check ran on a platform that may differ from this machine's OS, so \
work from the output and make the code correct on that platform. Check first that the base still \
has the fault: when another change already fixed it, say so and end your turn. Never weaken, skip \
or delete the failing test or lint. If the repair needs a spec decision, find its heading with \
graph_find and read it with graph_read (source: true). Claim the files, queue a type-check for the \
crate you change, and name the finished change with rosen commit -m. Velocity lands it, and the \
platform's next green post-land pass closes this failure. Keep the repair limited to this failure; \
do not take other work.";

/// A CI failure with what the prompt needs beyond the jobs.
pub struct CiFailure<'a> {
    /// The mirror's `owner/name` and the base branch on it.
    pub repository: &'a str,
    pub branch: &'a str,
    pub failure: &'a Failure,
    /// `gh run view --log-failed` for the run; empty for a commit status.
    pub log: &'a str,
    /// Where the whole of that log was saved.
    pub saved_log: Option<&'a Path>,
}

/// A Rosen per-platform post-land failure for GitHub-free base CI repairs (#591).
pub struct RosenCiFailure<'a> {
    pub project: &'a str,
    pub failure: &'a Failure,
    pub change: &'a str,
    pub platform: &'a str,
    pub check_kind: &'a str,
    pub output: &'a str,
    pub saved_log: Option<&'a Path>,
}

/// Saves a repair for a Rosen per-platform post-land failure and plans its thread (#591).
pub fn prepare_rosen_ci_repair(
    store: &mut StateStore,
    project: u32,
    report: &RosenCiFailure<'_>,
) -> Result<Option<(LaunchPlan, u32)>, String> {
    let current = store.project(project).ok_or("Unknown project")?;
    if !current.spec_locked {
        return Err("Lock the spec before starting a CI repair".into());
    }
    let failure = report.failure;
    let preferences = store.worktree_preferences(current);
    let prompt = rosen_ci_repair_prompt(report);
    let title = preview(
        &format!(
            "Fix CI: {} · post-land {}",
            report.platform, report.check_kind
        ),
        80,
    );
    let mut plan = plan_named_launch(
        current,
        &preferences,
        &store.settings(),
        &prompt,
        &title,
        &repair_slug(failure),
    )
    .map_err(|error| error.to_string())?;
    plan.title.request = None;
    plan.thread.role = velocity_core::tool_policy::ThreadRole::FollowUp;
    let thread = store
        .add_ci_repair(project, failure, plan.thread.clone(), &prompt)
        .map_err(|error| error.to_string())?;
    Ok(thread.map(|thread| (plan, thread)))
}

/// Builds the opening prompt for a Rosen per-platform post-land repair thread, naming the
/// platform, the culprit change/commit, and the failure output (#591).
pub fn rosen_ci_repair_prompt(report: &RosenCiFailure<'_>) -> String {
    let header = format!(
        "Rosen post-land check failed on the base.\n\
         Project: {}\n\
         Platform: {}\n\
         Check: post-land {}\n\
         Culprit change: {}\n\
         Culprit commit: {}\n",
        preview(report.project, 100),
        preview(report.platform, 80),
        preview(report.check_kind, 40),
        preview(report.change, 128),
        preview(&report.failure.commit, 64),
    );
    let mut footer = format!(
        "Full failure record: `rosen review failures --change {} --json`\n",
        preview(report.change, 128),
    );
    if let Some(saved) = report.saved_log {
        footer.push_str(&format!("Saved output log: {}\n", saved.display()));
    }
    let heading = "Failing check output:\n";
    let fixed = header.len() + heading.len() + footer.len() + ROSEN_INSTRUCTIONS.len() + 6;
    let budget = SHORT_PROMPT_BYTES.saturating_sub(fixed);
    let excerpt = if report.output.trim().is_empty() {
        "(Rosen recorded no output for this failure.)".to_owned()
    } else if report.output.len() <= budget {
        report.output.to_owned()
    } else {
        failure_excerpt(report.output, budget)
    };
    format!("{header}\n{heading}{excerpt}\n\n{footer}\n{ROSEN_INSTRUCTIONS}")
}

/// Saves a repair for each of the failure's jobs that has none and plans its thread.
/// `None` when every job already has an open repair.
pub fn prepare_ci_repair(
    store: &mut StateStore,
    project: u32,
    report: &CiFailure<'_>,
) -> Result<Option<(LaunchPlan, u32)>, String> {
    let current = store.project(project).ok_or("Unknown project")?;
    if !current.spec_locked {
        return Err("Lock the spec before starting a CI repair".into());
    }
    let failure = report.failure;
    let preferences = store.worktree_preferences(current);
    let prompt = ci_repair_prompt(report);
    let mut plan = plan_named_launch(
        current,
        &preferences,
        &store.settings(),
        &prompt,
        &repair_title(failure),
        &repair_slug(failure),
    )
    .map_err(|error| error.to_string())?;
    plan.title.request = None;
    plan.thread.role = velocity_core::tool_policy::ThreadRole::FollowUp;
    let thread = store
        .add_ci_repair(project, failure, plan.thread.clone(), &prompt)
        .map_err(|error| error.to_string())?;
    Ok(thread.map(|thread| (plan, thread)))
}

/// The launches of open repairs that were saved and never started, as after a crash or a
/// server that was down: the same threads with the same prompts, never new ones.
pub fn pending_ci_repairs(store: &StateStore, project: u32) -> Vec<(LaunchPlan, u32)> {
    let Some(current) = store.project(project) else {
        return Vec::new();
    };
    let preferences = store.worktree_preferences(current);
    let settings = store.settings();
    let mut threads: Vec<u32> = current
        .ci_repairs
        .iter()
        .map(|repair| repair.thread)
        .collect();
    threads.sort_unstable();
    threads.dedup();
    threads
        .into_iter()
        .filter_map(|identifier| {
            let thread = current
                .threads
                .iter()
                .find(|thread| thread.id == identifier)?;
            if thread.pending_rosen_launch.is_none()
                || !matches!(thread.state, ThreadState::Working | ThreadState::Error)
            {
                return None;
            }
            match plan_retry(current, thread, &preferences, &settings).ok()? {
                WorkspaceRetry::Thread(plan) => Some((*plan, identifier)),
                WorkspaceRetry::Goal(_) => None,
            }
        })
        .collect()
}

fn repair_title(failure: &Failure) -> String {
    let subject = match failure.jobs.as_slice() {
        [job] => base_ci::job_label(&failure.workflow, &job.job),
        jobs => format!("{} · {} jobs", failure.workflow, jobs.len()),
    };
    preview(&format!("Fix CI: {subject}"), 80)
}

/// Runs are numbered across the repository, so the run names the worktree. A status has
/// no run and is named for its commit.
fn repair_slug(failure: &Failure) -> String {
    if failure.run == 0 {
        format!(
            "fix-ci-{}",
            failure.commit.get(..12).unwrap_or(&failure.commit)
        )
    } else {
        format!("fix-ci-{}", failure.run)
    }
}

pub fn ci_repair_prompt(report: &CiFailure<'_>) -> String {
    let failure = report.failure;
    let mut header = format!(
        "GitHub CI failed on the base.\nRepository: {} · branch {}\nWorkflow: {}\nCommit: {}\nRun: {}\n",
        preview(report.repository, 100),
        preview(report.branch, 100),
        preview(&failure.workflow, 100),
        preview(&failure.commit, 64),
        preview(&failure.url, 200),
    );
    if !failure.detail.trim().is_empty() {
        header.push_str(&format!(
            "Status: {}\n",
            preview(failure.detail.trim(), 300)
        ));
    }
    let mut footer = String::new();
    let unlisted = failure.jobs.len().saturating_sub(EXCERPTED_JOBS);
    if unlisted > 0 {
        let names: Vec<&str> = failure.jobs[EXCERPTED_JOBS..]
            .iter()
            .map(|job| job.job.as_str())
            .collect();
        footer.push_str(&format!(
            "Also failed: {}\n",
            preview(&names.join(", "), 300)
        ));
    }
    if let Some(saved) = report.saved_log {
        footer.push_str(&format!(
            "The failed steps' whole log: {}\n",
            saved.display()
        ));
    }
    let shown = &failure.jobs[..failure.jobs.len().min(EXCERPTED_JOBS)];
    let job_sections: Vec<(String, String)> = shown
        .iter()
        .map(|job| {
            let log = base_ci::job_log(report.log, &job.job);
            let hung = job
                .hung_test
                .clone()
                .or_else(|| job.timed_out.then(|| base_ci::hung_test(&log)).flatten());
            (job_heading(job, hung.as_deref()), log)
        })
        .collect();
    let fixed = header.len()
        + footer.len()
        + INSTRUCTIONS.len()
        + job_sections
            .iter()
            .map(|(heading, _)| heading.len() + 3)
            .sum::<usize>()
        + 2;
    let each = SHORT_PROMPT_BYTES.saturating_sub(fixed) / shown.len().max(1);
    let mut prompt = header;
    for (heading, log) in &job_sections {
        let excerpt = if log.trim().is_empty() {
            "(GitHub gave no log for this job. Open the run.)".to_owned()
        } else if log.len() <= each {
            log.clone()
        } else {
            failure_excerpt(log, each)
        };
        prompt.push_str(&format!("\n{heading}\n{excerpt}\n"));
    }
    format!("{prompt}\n{footer}\n{INSTRUCTIONS}")
}

fn job_heading(job: &FailedJob, hung_test: Option<&str>) -> String {
    let mut heading = format!("Failed job: {}", preview(&job.job, 120));
    if job.timed_out {
        heading.push_str(" (timed out)");
    }
    if let Some(step) = &job.step {
        heading.push_str(&format!(" · step: {}", preview(step, 120)));
    }
    if let Some(runner) = &job.runner {
        heading.push_str(&format!(" · runs on {}", preview(runner, 80)));
    }
    if let Some(test) = hung_test {
        heading.push_str(&format!(
            "\nTest still running when the job stopped: {}",
            preview(test, 200)
        ));
    }
    heading
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#584): a timed-out CI job's repair brief must name the hung test taken from
    // the last "has been running for over 60 seconds" line of its job log so the fix-forward
    // thread targets the actual hang rather than a generic cancellation message.
    #[test]
    fn timed_out_repair_brief_names_the_hung_test() {
        let failure = Failure {
            workflow: "CI".into(),
            run: 99,
            commit: "520dcfc000000000000000000000000000000000".into(),
            url: "https://github.com/mESmaC/rosen/actions/runs/99".into(),
            detail: String::new(),
            jobs: vec![FailedJob {
                id: 555,
                job: "test (macos-latest)".into(),
                step: Some("Run tests".into()),
                runner: Some("macos-latest".into()),
                signature: None,
                timed_out: true,
                hung_test: None,
            }],
        };
        let log = "\
test (macos-latest)\tRun tests\t2026-11-21T20:10:00.0000000Z test early_slow has been running for over 60 seconds\n\
test (macos-latest)\tRun tests\t2026-11-21T20:11:00.0000000Z test early_slow ... ok\n\
test (macos-latest)\tRun tests\t2026-11-21T20:16:00.0000000Z test collapse_link::collapse_link_and_unlink_round_trip_with_live_server has been running for over 60 seconds\n\
test (macos-latest)\tRun tests\t2026-11-21T20:44:35.0000000Z ##[error]The operation was canceled.";
        let prompt = ci_repair_prompt(&CiFailure {
            repository: "mESmaC/rosen",
            branch: "main",
            failure: &failure,
            log,
            saved_log: None,
        });
        assert!(prompt.contains("Failed job: test (macos-latest) (timed out)"));
        assert!(prompt.contains(
            "Test still running when the job stopped: collapse_link::collapse_link_and_unlink_round_trip_with_live_server"
        ));
    }

    // Risk (#591): a macOS-only Rosen post-land failure repair brief must name the platform,
    // the culprit change/commit, and the failing output without referring to GitHub Actions.
    #[test]
    fn rosen_per_platform_repair_brief_names_platform_culprit_and_output() {
        let failure = Failure {
            workflow: base_ci::ROSEN_BASE_WORKFLOW.into(),
            run: 42,
            commit: "520dcfc000000000000000000000000000000000".into(),
            url: String::new(),
            detail: "Culprit change: chg-macos".into(),
            jobs: vec![FailedJob {
                id: 42,
                job: "macOS · test".into(),
                step: Some("post-land test".into()),
                runner: Some("macOS".into()),
                signature: None,
                timed_out: false,
                hung_test: None,
            }],
        };
        let prompt = rosen_ci_repair_prompt(&RosenCiFailure {
            project: "rosen",
            failure: &failure,
            change: "chg-macos",
            platform: "macOS",
            check_kind: "test",
            output: "test kqueue_watcher::sees_rename ... FAILED\n",
            saved_log: None,
        });
        assert!(prompt.contains("Platform: macOS"));
        assert!(prompt.contains("Culprit change: chg-macos"));
        assert!(prompt.contains("Culprit commit: 520dcfc000000000000000000000000000000000"));
        assert!(prompt.contains("kqueue_watcher::sees_rename ... FAILED"));
        assert!(!prompt.contains("GitHub"));
    }
}
