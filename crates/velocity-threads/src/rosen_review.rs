//! The Rosen side of PR duty's runner (F5, F7): which changes are ready for review, and
//! preparing a change's review folder. A change is ready once its thread named its work
//! (`rosen commit -m`), and stays so until it lands or changes are requested.

use std::path::{Path, PathBuf};

use velocity_core::merge_gate::ReviewMode;
use velocity_core::pr_review::{ReviewCandidate, ReviewWork};
use velocity_core::review_diff::DiffSummary;
use velocity_core::review_tier::{TierAssessment, assess_tier_with};
use velocity_core::rosen_checker::change_review_mode;
use velocity_rosen::ProjectServer;
use velocity_rosen::duty;
use velocity_rosen::policy::{CheckPolicy, policy_at};
use velocity_store::duty::PrDutyRoster;
use velocity_store::{ReviewStage, ThreadState};

use crate::review::{ReviewPlan, plan_change_review};
use crate::rosen_duty_server::{RosenDutyLaunch, check_out_review};

/// The project's Rosen changes in review, each with whether Rosen holds an approval at its
/// head.
pub(crate) async fn change_candidates(
    app_data: &Path,
    roster: &PrDutyRoster,
) -> Result<Vec<ReviewCandidate>, String> {
    let ready: Vec<(u32, String, String)> = roster
        .project
        .threads
        .iter()
        .filter(|thread| {
            thread.state == ThreadState::Review && thread.review_stage == Some(ReviewStage::Review)
        })
        .filter_map(|thread| {
            Some((
                thread.id,
                thread.rosen_change.clone()?,
                thread.title.clone(),
            ))
        })
        .collect();
    if ready.is_empty() {
        return Ok(Vec::new());
    }
    let server = ProjectServer::new(app_data, roster.project.id);
    let mut candidates = Vec::new();
    for (thread, change, title) in ready {
        let standing = duty::change_standing(&server, &change)
            .await
            .map_err(|error| format!("Rosen: {error}"))?;
        if standing.open {
            let candidate =
                ReviewCandidate::change(thread, change, title, standing.head, standing.approved);
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

/// A change's review folder, written and set up, with what the review is told about it.
pub(crate) struct PreparedChange {
    pub plan: ReviewPlan,
    pub launch_file: PathBuf,
    pub head: String,
    pub base: String,
    pub conflicts: Vec<String>,
    pub tier: TierAssessment,
    pub mode: ReviewMode,
}

/// Where one review's agent and its change are, for [`prepare_change`].
pub(crate) struct ChangeReview<'a> {
    pub app_data: &'a Path,
    pub roster: &'a PrDutyRoster,
    pub nickname: &'a str,
    pub thread: u32,
    pub change: &'a str,
    pub work: ReviewWork,
    pub report: &'a Path,
}

/// `change`'s review tier, from its paths, and how it is reviewed (F7 "Checks before
/// merge"): by reading when Velocity can check it itself, with the project's pre-land
/// commands. Merge work is an approved head Velocity could not check, so its agent does.
///
/// The tier and the commands come from the policy at the current base, as Rosen reads it
/// from `rosen.toml` there: never from the change's own copy, which could drop a sensitive
/// path, nor from a git checkout's `AGENTS.md`, which can be older than the base. A policy
/// Rosen can't read is an error, so the review waits for the next tick rather than going
/// ahead as if nothing were sensitive.
pub(crate) async fn change_mode(
    server: &ProjectServer,
    change: &str,
    work: ReviewWork,
) -> Result<(TierAssessment, ReviewMode), String> {
    let paths = duty::changed_paths(server, change)
        .await
        .map_err(|error| format!("Could not read the change's diff: {error}"))?;
    let policy = policy_at(server, None)
        .await
        .map_err(|error| format!("Could not read the project's rosen.toml at the base: {error}"))?;
    Ok(mode_under(&policy, &paths, work))
}

/// The tier and review mode of a change touching `paths`, under `policy`.
pub(crate) fn mode_under(
    policy: &CheckPolicy,
    paths: &[String],
    work: ReviewWork,
) -> (TierAssessment, ReviewMode) {
    let tier = assess_tier_with(&DiffSummary::from_paths(paths.to_vec()), |path| {
        policy.is_sensitive(path)
    });
    if work == ReviewWork::Merge {
        return (tier, ReviewMode::LocalChecks);
    }
    let mode = change_review_mode(tier.tier, paths, !policy.commands.is_empty());
    (tier, mode)
}

/// Plans the review, writes the change's merge result into its folder with the folder
/// setup, tiers it by its paths and writes the MCP server's launch file.
pub(crate) async fn prepare_change(review: ChangeReview<'_>) -> Result<PreparedChange, String> {
    let agent = review
        .roster
        .agents
        .iter()
        .find(|agent| agent.nickname == review.nickname)
        .ok_or("The agent left PR duty")?;
    let plan = plan_change_review(review.roster, agent, review.thread, review.change)
        .map_err(|error| error.to_string())?;
    let server = ProjectServer::new(review.app_data, review.roster.project.id);
    let (tier, mode) = change_mode(&server, review.change, review.work).await?;
    let launch = RosenDutyLaunch::for_review(review.app_data, &plan, review.change, review.report)
        .by_reading(mode == ReviewMode::Reading);
    let checkout = check_out_review(&server, &launch).await?;
    let launch_file = review.report.with_extension("rosen.json");
    let text = serde_json::to_string(&launch).map_err(|error| error.to_string())?;
    std::fs::write(&launch_file, text)
        .map_err(|error| format!("Could not write {}: {error}", launch_file.display()))?;
    Ok(PreparedChange {
        plan,
        launch_file,
        head: checkout.head,
        base: checkout.base,
        conflicts: checkout.conflicts,
        tier,
        mode,
    })
}

/// Removes the review folder once its agent has stopped; the next review writes it afresh.
pub(crate) async fn remove_folder(folder: PathBuf) {
    let _ = tokio::task::spawn_blocking(move || duty::remove_checkout(&folder)).await;
}
