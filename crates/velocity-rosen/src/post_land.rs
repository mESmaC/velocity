//! Post-land verification (Rosen R12; SPEC.md F7 "After a land"): the calls Velocity's
//! checker makes to learn which landed commit to test, report what the tests did, and walk
//! the bisection Rosen runs after a failure.
//!
//! Rosen decides everything: which commit is tested next, how a failing interval is split,
//! when a culprit is confirmed, and the revert. A checker only runs the commands on the
//! commit it is offered and says whether they passed.

use std::path::Path;

use rosen_client::protocol::{
    BisectJob, BisectResult, BisectState, ErrorCode, LogRequest, PostLandJob, PostLandWork,
};
use rosen_client::{Client, ObjectId};
use rosen_core::project_config::{RecheckMode, ReviewMode};

pub use rosen_client::protocol::{CheckKind, CheckPriority};

use crate::{ProjectServer, RosenError, duty};

/// The newest landed commit this check kind has not covered, and the commands to run on it.
#[derive(Clone, Debug)]
pub struct Batch {
    pub commit: String,
    pub kind: CheckKind,
    /// Rosen's run-queue class, preserved independently of the check kind.
    pub priority: CheckPriority,
    /// This land used or enabled after-land mode. A mode-changing land still needs
    /// verification even when its own configuration turns the mode off.
    pub after_land: bool,
    /// The build or test check set the project's `rosen.toml` held at that commit.
    pub commands: Vec<String>,
    job: PostLandJob,
}

/// One commit Rosen wants tested to find the change that broke a batch.
#[derive(Clone, Debug)]
pub struct Step {
    pub commit: String,
    job: BisectJob,
}

/// Where a bisection stands after a result.
#[derive(Clone, Debug)]
pub enum Bisection {
    /// Rosen wants this commit tested next.
    Step(Step),
    /// One change failed twice where its parent passed. Rosen either reverted it or
    /// recorded a test failure, according to the policy that authorised its land.
    Culprit {
        change: String,
        commit: String,
        kind: CheckKind,
        reverted: bool,
    },
    /// The results contradict each other, so no change is blamed and nothing is reverted.
    Unconfirmed,
}

/// The batch to test now; `None` when every landed commit is covered.
pub async fn next_batch(server: &ProjectServer) -> Result<Option<Batch>, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    batch(&client, client.post_land_next().await?).await
}

/// The independent build prefix, including merge-mode checks after a before-land gate.
pub async fn next_build_batch(server: &ProjectServer) -> Result<Option<Batch>, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    // Merge mode can route post_land_next to builds even on a before-land project.
    // Its build worker must advance that prefix so the test worker can receive tests.
    let merge = client.project_config(None).await?.config.recheck == RecheckMode::Merge;
    Ok(batch(&client, client.post_land_build_next().await?)
        .await?
        .filter(|batch| batch.after_land || merge || batch.priority == CheckPriority::Landing))
}

/// The current base's landing policy, never a checkout's possibly stale configuration.
pub async fn after_land(server: &ProjectServer) -> Result<bool, RosenError> {
    after_land_at(server, None).await
}

pub async fn after_land_at(server: &ProjectServer, base: Option<&str>) -> Result<bool, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let at = base.map(crate::policy::object).transpose()?;
    Ok(client.project_config(at).await?.config.review == ReviewMode::AfterLand)
}

/// Confirmed, non-reverting test failures retained by Rosen, including across restarts.
pub async fn test_failures(server: &ProjectServer) -> Result<Vec<TestFailure>, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let failures = match client.post_land_failures(None).await {
        Ok(failures) => failures,
        // Older before-land servers have no inventory request; their verifier still runs.
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::UnknownRequest => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };
    let mut reports = Vec::new();
    for failure in failures
        .into_iter()
        .filter(|failure| failure.kind == CheckKind::Test && failure.revert.is_none())
    {
        // A before-land failure is recorded before its revert completes. Only the policy
        // that authorised its land can distinguish it from a non-reverting test failure.
        let request = LogRequest {
            at: Some(failure.commit.to_string()),
            limit: Some(1),
            ..LogRequest::default()
        };
        let history = client.log(request).await?;
        let parent = history
            .entries
            .first()
            .and_then(|entry| entry.parents.first())
            .ok_or_else(|| RosenError::InvalidId(failure.commit.to_string()))?;
        if client.project_config(Some(*parent)).await?.config.review != ReviewMode::AfterLand {
            continue;
        }
        reports.push(TestFailure {
            session: failure.session,
            change: failure.change,
            commit: failure.commit.to_string(),
            output: failure.output,
        });
    }
    Ok(reports)
}

#[derive(Clone, Debug)]
pub struct TestFailure {
    pub session: i64,
    pub change: String,
    pub commit: String,
    pub output: String,
}

/// Every command passed on the batch's commit: it and every land before it are green.
pub async fn batch_passed(server: &ProjectServer, batch: &Batch) -> Result<(), RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    client.post_land_passed(batch.job.clone()).await?;
    Ok(())
}

/// A command failed on the batch's commit, printing `output`. Rosen starts a bisection
/// from the last green commit, or carries on with the one it already has for this commit.
pub async fn batch_failed(
    server: &ProjectServer,
    batch: &Batch,
    output: &str,
) -> Result<Bisection, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let state = client
        .post_land_failed(batch.job.clone(), output.to_owned())
        .await?;
    bisection(&client, state).await
}

/// Reports what the commands did on a bisection step's commit.
pub async fn step_result(
    server: &ProjectServer,
    step: &Step,
    passed: bool,
    output: &str,
) -> Result<Bisection, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let result = BisectResult {
        job: step.job.clone(),
        passed,
        output: output.to_owned(),
    };
    bisection(&client, client.bisect_submit(result).await?).await
}

/// Writes landed commit `commit`'s tree beside `folder`, ready for
/// [`crate::checker::apply_staged`]. A folder Velocity did not check out is never touched.
pub async fn stage_commit(
    server: &ProjectServer,
    commit: &str,
    folder: &Path,
) -> Result<(), RosenError> {
    let staging = duty::claim_checkout(folder)?;
    let client = Client::connect(server.endpoint()).await?;
    let request = LogRequest {
        at: Some(commit.to_owned()),
        paths: Vec::new(),
        limit: Some(1),
    };
    let tree: ObjectId = client
        .log(request)
        .await?
        .entries
        .first()
        .filter(|entry| entry.commit.to_string() == commit)
        .map(|entry| entry.tree)
        .ok_or_else(|| RosenError::InvalidId(commit.to_owned()))?;
    client
        .materialise_tree(tree, &staging, Default::default())
        .await?;
    Ok(())
}

async fn batch(client: &Client, work: PostLandWork) -> Result<Option<Batch>, RosenError> {
    let Some(job) = work.job else {
        return Ok(None);
    };
    let policy = client.project_config(Some(job.commit)).await?;
    let request = LogRequest {
        at: Some(job.commit.to_string()),
        limit: Some(1),
        ..LogRequest::default()
    };
    let history = client.log(request).await?;
    let parent = history
        .entries
        .first()
        .and_then(|entry| entry.parents.first())
        .ok_or_else(|| RosenError::InvalidId(job.commit.to_string()))?;
    let authorising = client.project_config(Some(*parent)).await?;
    Ok(Some(Batch {
        commit: job.commit.to_string(),
        kind: job.kind,
        priority: job.priority,
        after_land: policy.config.review == ReviewMode::AfterLand
            || authorising.config.review == ReviewMode::AfterLand,
        commands: job.check_set.commands.clone(),
        job,
    }))
}

async fn bisection(client: &Client, state: BisectState) -> Result<Bisection, RosenError> {
    let kind = state.kind;
    Ok(match (state.culprit, state.job) {
        (Some(culprit), _) => {
            // A build requests an inverse, but an overlapping later land can refuse it.
            // Only the durable failure record proves Rosen actually reverted it.
            let reverted = client
                .post_land_failures(Some(culprit.change.clone()))
                .await?
                .iter()
                .any(|failure| {
                    failure.commit == culprit.commit
                        && failure.kind == kind
                        && failure.revert.is_some()
                });
            Bisection::Culprit {
                change: culprit.change,
                commit: culprit.commit.to_string(),
                kind,
                reverted,
            }
        }
        (None, Some(job)) if !state.flaky => Bisection::Step(Step {
            commit: job.commit.to_string(),
            job,
        }),
        _ => Bisection::Unconfirmed,
    })
}
