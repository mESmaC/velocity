//! Post-land verification (Rosen R12; SPEC.md F7 "After a land"): the calls Velocity's
//! checker makes to learn which landed commit to test, report what the tests did, and walk
//! the bisection Rosen runs after a failure.
//!
//! Rosen decides everything: which commit is tested next, how a failing interval is split,
//! when a culprit is confirmed, and the revert. A checker only runs the commands on the
//! commit it is offered and says whether they passed.

use std::path::Path;

use rosen_client::protocol::{
    BisectJob, BisectResult, BisectState, LogRequest, PostLandJob, PostLandWork,
};
use rosen_client::{Client, ObjectId};

use crate::{ProjectServer, RosenError, duty};

/// The newest landed commit no full run has covered yet, and the commands to run on it.
#[derive(Clone, Debug)]
pub struct Batch {
    pub commit: String,
    /// The post-land check set the project's `rosen.toml` held at that commit.
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
    /// One change failed twice where its parent passed: Rosen has landed its revert and
    /// reopened it for its owner.
    Culprit { change: String, commit: String },
    /// The results contradict each other, so no change is blamed and nothing is reverted.
    Unconfirmed,
}

/// The batch to test now; `None` when every landed commit is covered.
pub async fn next_batch(server: &ProjectServer) -> Result<Option<Batch>, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    Ok(batch(client.post_land_next().await?))
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
    Ok(bisection(state))
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
    Ok(bisection(client.bisect_submit(result).await?))
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

fn batch(work: PostLandWork) -> Option<Batch> {
    work.job.map(|job| Batch {
        commit: job.commit.to_string(),
        commands: job.check_set.commands.clone(),
        job,
    })
}

fn bisection(state: BisectState) -> Bisection {
    match (state.culprit, state.job) {
        (Some(culprit), _) => Bisection::Culprit {
            change: culprit.change,
            commit: culprit.commit.to_string(),
        },
        (None, Some(job)) if !state.flaky => Bisection::Step(Step {
            commit: job.commit.to_string(),
            job,
        }),
        _ => Bisection::Unconfirmed,
    }
}
