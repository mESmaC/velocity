//! Post-land verification (Rosen R12; SPEC.md F7 "After a land"): the calls Velocity's
//! checker makes to learn which landed commit to test, report what the tests did, and walk
//! the bisection Rosen runs after a failure.
//!
//! Rosen decides everything: which commit is tested next, how a failing interval is split,
//! when a culprit is confirmed, and the revert. A checker only runs the commands on the
//! commit it is offered and says whether they passed.

use std::path::Path;

use rosen_client::protocol::{
    BisectJob, BisectResult, BisectState, DiffRequest, ErrorCode, LogRequest, PostLandJob,
    PostLandWork, ReadAnswer,
};
use rosen_client::{AgentId, ChangeParent, Client, ObjectId};
use rosen_core::project_config::{CONFIG_PATH, ProjectConfig, RecheckMode, ReviewMode};

pub use rosen_client::protocol::{CheckKind, CheckPriority};

use crate::{ProjectServer, RosenError, checkout, integration};

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
    merge_build: Option<PostLandJob>,
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
    let client = crate::connection::requests(server.endpoint()).await?;
    let host = rosen_core::Platform::host();
    let mut work = match client.post_land_next_for(host.as_str()).await {
        Ok(work) => work,
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::UnknownRequest => {
            client.post_land_next().await?
        }
        Err(error) => return Err(error.into()),
    };
    if work
        .job
        .as_ref()
        .is_some_and(|job| job.kind == CheckKind::Build)
        && after_land(server).await?
    {
        // Merge rechecking makes Rosen offer builds first. The integration pass
        // performs those commands and reports both prefixes after the full pass.
        work.job = integration::merge_test_job(server, &client).await?;
    }
    batch(&client, work).await
}

/// The newest landed commit on the base that Rosen's `post_land_green` has not covered
/// yet, even if a past non-reverting test failure suppressed `post_land_next` (#567).
pub async fn unverified_tip_batch(server: &ProjectServer) -> Result<Option<Batch>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let job = integration::tip_test_job(server, &client).await?;
    batch(
        &client,
        PostLandWork {
            job,
            last_green: None,
        },
    )
    .await
}

/// The independent build prefix, including merge-mode checks after a before-land gate.
pub async fn next_build_batch(server: &ProjectServer) -> Result<Option<Batch>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    // Merge mode can route post_land_next to builds even on a before-land project.
    // Its build worker must advance that prefix so the test worker can receive tests.
    let merge = client.project_config(None).await?.config.recheck == RecheckMode::Merge;
    let host = rosen_core::Platform::host();
    let work = match client.post_land_build_next_for(host.as_str()).await {
        Ok(work) => work,
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::UnknownRequest => {
            client.post_land_build_next().await?
        }
        Err(error) => return Err(error.into()),
    };
    Ok(batch(&client, work)
        .await?
        .filter(|batch| !batch.after_land && (merge || batch.priority == CheckPriority::Landing)))
}

/// The current base's landing policy, never a checkout's possibly stale configuration.
pub async fn after_land(server: &ProjectServer) -> Result<bool, RosenError> {
    after_land_at(server, None).await
}

pub async fn after_land_at(server: &ProjectServer, base: Option<&str>) -> Result<bool, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let at = base.map(crate::policy::object).transpose()?;
    Ok(client.project_config(at).await?.config.review == ReviewMode::AfterLand)
}

/// Whether `proposed_toml` changes only `review` from `before-land` to `after-land`.
/// Comparing the parsed `ProjectConfig` against the parent base ensures a migration change
/// cannot alter sensitive paths, checks or other settings under cover of switching modes.
pub fn is_after_land_switch(base: &ProjectConfig, proposed_toml: &str) -> bool {
    if base.review != ReviewMode::BeforeLand {
        return false;
    }
    let Ok(proposed) = ProjectConfig::parse(proposed_toml) else {
        return false;
    };
    let mut expected = base.clone();
    expected.review = ReviewMode::AfterLand;
    proposed == expected
}

/// Whether `change` sits directly on a base commit, touches only `rosen.toml` and switches
/// `review` from `before-land` to `after-land` without altering any other setting (#524).
pub async fn switches_to_after_land(
    server: &ProjectServer,
    change: &str,
) -> Result<bool, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let record = client.change_show(change).await?.change;
    let ChangeParent::Base(parent_base) = record.parent else {
        return Ok(false);
    };
    let diff = client
        .diff(DiffRequest {
            to: Some(change.to_owned()),
            context: Some(0),
            ..DiffRequest::default()
        })
        .await?;
    if diff.files.len() != 1 || diff.files[0].path != CONFIG_PATH {
        return Ok(false);
    }
    let Some(proposed_toml) = read_proposed_config(&client, record.head).await? else {
        return Ok(false);
    };
    let base_config = client.project_config(Some(parent_base)).await?.config;
    Ok(is_after_land_switch(&base_config, &proposed_toml))
}

async fn read_proposed_config(
    client: &Client,
    head: ObjectId,
) -> Result<Option<String>, RosenError> {
    match client.workspace_read(&head.to_string(), CONFIG_PATH).await {
        Ok(ReadAnswer::File { mode, data, .. }) if matches!(mode.as_str(), "100644" | "100755") => {
            Ok(String::from_utf8(data).ok())
        }
        Ok(_) => Ok(None),
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Confirmed, non-reverting test failures retained by Rosen, including across restarts.
pub async fn test_failures(server: &ProjectServer) -> Result<Vec<TestFailure>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let failures = match client.post_land_failures(None).await {
        Ok(failures) => failures,
        // Older before-land servers have no inventory request; their verifier still runs.
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::UnknownRequest => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };
    let mut reports = Vec::new();
    for failure in failures.into_iter().filter(|failure| {
        failure.kind == CheckKind::Test
            && failure.revert.is_none()
            && !failure.output.starts_with(integration::FAILURE_HEADING)
    }) {
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

/// Any confirmed post-land failure retained by Rosen (build, test or integration),
/// whether reverted or fixed forward (#540).
#[derive(Clone, Debug)]
pub struct ConfirmedFailure {
    pub session: i64,
    pub change: String,
    pub commit: String,
    pub kind: CheckKind,
    pub output: String,
    pub reverted: bool,
    pub integration: bool,
}

/// Every confirmed post-land failure retained by Rosen, for automatic discovery logging (#540).
pub async fn confirmed_failures(
    server: &ProjectServer,
) -> Result<Vec<ConfirmedFailure>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let failures = match client.post_land_failures(None).await {
        Ok(failures) => failures,
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::UnknownRequest => {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error.into()),
    };
    Ok(failures
        .into_iter()
        .map(|failure| ConfirmedFailure {
            session: failure.session,
            change: failure.change,
            commit: failure.commit.to_string(),
            kind: failure.kind,
            integration: failure.output.starts_with(integration::FAILURE_HEADING),
            output: failure.output,
            reverted: failure.revert.is_some(),
        })
        .collect())
}

/// Every command passed on the batch's commit: it and every land before it are green.
pub async fn batch_passed(server: &ProjectServer, batch: &Batch) -> Result<(), RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    if let Some(job) = &batch.merge_build {
        client.post_land_passed(job.clone()).await?;
    }
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
    let client = crate::connection::requests(server.endpoint()).await?;
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
    let client = crate::connection::requests(server.endpoint()).await?;
    let result = BisectResult {
        job: step.job.clone(),
        passed,
        output: output.to_owned(),
        // Rosen records the checker's nick with the connection's machine (R14).
        submitter: AgentId::new(crate::checker::CHECKER).ok(),
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
    let staging = checkout::claim_checkout(folder)?;
    let client = crate::connection::requests(server.endpoint()).await?;
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
    let after_land = policy.config.review == ReviewMode::AfterLand
        || authorising.config.review == ReviewMode::AfterLand;
    let mut commands = job.check_set.commands.clone();
    let mut merge_build = None;
    if after_land && job.kind == CheckKind::Test {
        let (pre_land_commands, build_job) = resolve_pre_land_job(job.commit, &policy.config);
        if !commands
            .iter()
            .any(|command| command == integration::FULL_SUITE)
        {
            commands.splice(..0, pre_land_commands);
        }
        // The integration pass runs the pre-land build/lint suite alongside post-land tests,
        // so advancing both prefixes keeps `post_land_build_green` (`built`) in sync (#567).
        merge_build = build_job;
    }
    Ok(Some(Batch {
        commit: job.commit.to_string(),
        kind: job.kind,
        priority: job.priority,
        after_land,
        commands,
        job,
        merge_build,
    }))
}

fn resolve_pre_land_job(
    commit: ObjectId,
    policy: &ProjectConfig,
) -> (Vec<String>, Option<PostLandJob>) {
    let mut pre_land = policy.checks.pre_land.clone();
    let target = if pre_land.platforms.is_empty() {
        None
    } else {
        let Some(target) = pre_land
            .matching_target(&rosen_core::Platform::host())
            .cloned()
        else {
            return (Vec::new(), None);
        };
        Some(target)
    };
    let commands = pre_land.commands_for(target.as_ref());
    let job = (!commands.is_empty() || policy.recheck == RecheckMode::Merge).then(|| {
        let priority = if policy.recheck == RecheckMode::Merge {
            CheckPriority::Landing
        } else {
            CheckPriority::Verify
        };
        pre_land.commands = commands.clone();
        PostLandJob {
            commit,
            kind: CheckKind::Build,
            priority,
            check_set: pre_land,
            platform: target.map(|platform| platform.as_str().to_owned()),
            submitter: None,
        }
    });
    (commands, job)
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

/// Base commit covering one or more orphaned lands that need post-land verification (#519).
#[derive(Clone, Debug)]
pub struct OrphanedBatch {
    pub commit: String,
    pub after_land: bool,
    pub commands: Vec<String>,
    /// The subset of requested orphaned commits present in `commit`'s history.
    pub covered: Vec<String>,
}

/// How a candidate commit relates to the running build's commit in Rosen's history (#551).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitAncestry {
    /// `candidate` strictly descends from `running`.
    Descendant,
    /// `candidate` and `running` are the same commit.
    Same,
    /// `candidate` is a strict ancestor of `running`.
    Ancestor,
    /// Both commits exist in history, and neither is an ancestor of the other.
    Diverged,
    /// Either commit could not be resolved or is absent from this server's history.
    Unknown,
}

/// Compares `candidate` against `running` in this project's Rosen history (#551).
/// Accepts a 40-character commit ID or an unambiguous short prefix for `running` (older
/// local builds embedded only a 7-character commit in their version string).
pub async fn compare_commits(
    server: &ProjectServer,
    running: &str,
    candidate: &str,
) -> Result<CommitAncestry, RosenError> {
    let running_lower = running.trim().to_ascii_lowercase();
    let candidate_lower = candidate.trim().to_ascii_lowercase();
    let Ok(candidate_id) = crate::policy::object(&candidate_lower) else {
        return Ok(CommitAncestry::Unknown);
    };
    let Some(running_full) = resolve_commit_prefix(server, &running_lower).await? else {
        return Ok(CommitAncestry::Unknown);
    };
    let Ok(running_id) = crate::policy::object(&running_full) else {
        return Ok(CommitAncestry::Unknown);
    };
    if running_id == candidate_id {
        return Ok(CommitAncestry::Same);
    }
    let client = server.connect().await?;
    match client.merge_base(&running_full, &candidate_lower).await {
        Ok(answer) => Ok(match answer.merge_base {
            Some(base) if base == running_id => CommitAncestry::Descendant,
            Some(base) if base == candidate_id => CommitAncestry::Ancestor,
            Some(_) => CommitAncestry::Diverged,
            None => CommitAncestry::Unknown,
        }),
        Err(rosen_client::Error::Server(_)) => Ok(CommitAncestry::Unknown),
        Err(error) => Err(error.into()),
    }
}

async fn resolve_commit_prefix(
    server: &ProjectServer,
    prefix: &str,
) -> Result<Option<String>, RosenError> {
    if prefix.len() == 40 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(Some(prefix.to_owned()));
    }
    if !(7..=40).contains(&prefix.len()) || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let metadata = server.data_dir().join("metadata.sqlite");
    let prefix = prefix.to_owned();
    tokio::task::spawn_blocking(move || read_unique_history_prefix(&metadata, &prefix))
        .await
        .map_err(|error| RosenError::StatusData(error.to_string()))?
}

fn read_unique_history_prefix(metadata: &Path, prefix: &str) -> Result<Option<String>, RosenError> {
    use rusqlite::{Connection, OpenFlags};
    if !metadata.is_file() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(metadata, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let has_history: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'base_history')",
        [],
        |row| row.get(0),
    )?;
    if !has_history {
        return Ok(None);
    }
    let pattern = format!("{prefix}%");
    let mut statement = connection
        .prepare("SELECT DISTINCT commit_id FROM base_history WHERE commit_id LIKE ?1 LIMIT 2")?;
    let matches: Vec<String> = statement
        .query_map([pattern], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    Ok((matches.len() == 1)
        .then(|| matches.into_iter().next())
        .flatten())
}

/// One commit landed on the base strictly after the running build (#601).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandedCommitSummary {
    pub commit: String,
    pub summary: String,
}

/// Whether `container` contains `target` in this project's Rosen history (#601).
/// Both arguments accept a 40-character commit ID or an unambiguous 7..=40 hex prefix.
pub async fn commit_contains(
    server: &ProjectServer,
    container: &str,
    target: &str,
) -> Result<bool, RosenError> {
    let container_lower = container.trim().to_ascii_lowercase();
    let target_lower = target.trim().to_ascii_lowercase();
    let Some(container_full) = resolve_commit_prefix(server, &container_lower).await? else {
        return Ok(false);
    };
    let Some(target_full) = resolve_commit_prefix(server, &target_lower).await? else {
        return Ok(false);
    };
    if container_full == target_full {
        return Ok(true);
    }
    let client = server.connect().await?;
    is_ancestor(&client, &target_full, &container_full).await
}

/// Base commits landed strictly after `running`, newest first, capped at `limit` (#601).
/// Returns an empty list when `running` is unknown or already at/ahead of the base tip.
pub async fn commits_since(
    server: &ProjectServer,
    running: &str,
    limit: u32,
) -> Result<Vec<LandedCommitSummary>, RosenError> {
    let running_lower = running.trim().to_ascii_lowercase();
    let Some(running_full) = resolve_commit_prefix(server, &running_lower).await? else {
        return Ok(Vec::new());
    };
    let Ok(running_id) = crate::policy::object(&running_full) else {
        return Ok(Vec::new());
    };
    let client = server.connect().await?;
    let log = client
        .log(LogRequest {
            at: None,
            paths: Vec::new(),
            limit: Some(limit),
        })
        .await?;
    let Some(tip) = log.entries.first() else {
        return Ok(Vec::new());
    };
    if tip.commit == running_id {
        return Ok(Vec::new());
    }
    if !is_ancestor(&client, &running_full, &tip.commit.to_string()).await? {
        return Ok(Vec::new());
    }
    let mut newer = Vec::new();
    for entry in log.entries {
        if entry.commit == running_id {
            break;
        }
        newer.push(LandedCommitSummary {
            commit: entry.commit.to_string(),
            summary: entry.summary,
        });
    }
    Ok(newer)
}

/// Whether `commit` is already in this server's local base history (#519).
pub async fn base_contains_commit(
    server: &ProjectServer,
    commit: &str,
) -> Result<bool, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let Some((_, tip)) = base_tip(&client).await? else {
        return Ok(false);
    };
    is_ancestor(&client, commit, &tip).await
}

/// Resolves the base tip and check commands to verify orphaned landed commits (#519).
/// Catches up from the mirror once when any requested commit is not yet in the local base.
pub async fn orphaned_batch(
    server: &ProjectServer,
    commits: &[String],
) -> Result<Option<OrphanedBatch>, RosenError> {
    if commits.is_empty() {
        return Ok(None);
    }
    let client = crate::connection::requests(server.endpoint()).await?;
    let Some(mut tip) = base_tip(&client).await? else {
        return Ok(None);
    };
    let mut covered = ancestors_of(&client, commits, &tip.1).await?;
    if covered.len() < commits.len()
        && matches!(
            crate::catch_up_from_mirror(server).await,
            Ok(crate::CatchUp::FastForwarded(_))
        )
        && let Some(updated) = base_tip(&client).await?
    {
        tip = updated;
        covered = ancestors_of(&client, commits, &tip.1).await?;
    }
    if covered.is_empty() {
        return Ok(None);
    }
    let policy = client.project_config(Some(tip.0)).await?;
    let after_land = policy.config.review == ReviewMode::AfterLand;
    let mut commands = policy.config.checks.post_land.commands.clone();
    if after_land
        && !commands
            .iter()
            .any(|command| command == integration::FULL_SUITE)
    {
        commands.splice(..0, policy.config.checks.pre_land.commands.iter().cloned());
    }
    Ok(Some(OrphanedBatch {
        commit: tip.1,
        after_land,
        commands,
        covered,
    }))
}

async fn base_tip(client: &Client) -> Result<Option<(ObjectId, String)>, RosenError> {
    let request = LogRequest {
        at: None,
        paths: Vec::new(),
        limit: Some(1),
    };
    Ok(client
        .log(request)
        .await?
        .entries
        .into_iter()
        .next()
        .map(|entry| (entry.commit, entry.commit.to_string())))
}

async fn ancestors_of(
    client: &Client,
    commits: &[String],
    tip: &str,
) -> Result<Vec<String>, RosenError> {
    let mut covered = Vec::new();
    for commit in commits {
        if is_ancestor(client, commit, tip).await? {
            covered.push(commit.clone());
        }
    }
    Ok(covered)
}

async fn is_ancestor(client: &Client, commit: &str, tip: &str) -> Result<bool, RosenError> {
    let Ok(identifier) = crate::policy::object(commit) else {
        return Ok(false);
    };
    match client.merge_base(commit, tip).await {
        Ok(answer) => Ok(answer.merge_base == Some(identifier)),
        Err(rosen_client::Error::Server(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// The immutable Rosen base snapshot used by the code graph.
pub struct BaseSnapshot {
    client: std::sync::Arc<crate::connection::Connection>,
    commit: String,
    tree: ObjectId,
}

impl BaseSnapshot {
    pub async fn read(server: &ProjectServer) -> Result<Self, RosenError> {
        let client = server.connect().await?;
        let request = LogRequest {
            at: None,
            paths: Vec::new(),
            limit: Some(1),
        };
        let entry = client
            .log(request)
            .await?
            .entries
            .into_iter()
            .next()
            .ok_or_else(|| RosenError::InvalidId("base has no commits".into()))?;
        Ok(Self {
            client,
            commit: entry.commit.to_string(),
            tree: entry.tree,
        })
    }

    pub fn commit(&self) -> &str {
        &self.commit
    }

    pub async fn materialise(&self, folder: &Path) -> Result<(), RosenError> {
        self.client
            .materialise_tree(self.tree, folder, Default::default())
            .await?;
        Ok(())
    }
}

/// Reads Rosen's per-platform post-land checkpoints and confirmed failures for the
/// GitHub-free base CI watcher (#591).
pub async fn base_ci_snapshot(
    server: &ProjectServer,
) -> Result<velocity_core::base_ci::RosenBaseSnapshot, RosenError> {
    use velocity_core::base_ci::{
        RosenBaseSnapshot, RosenPlatformCheckpoint, RosenPostLandFailure,
    };

    let client = crate::connection::requests(server.endpoint()).await?;
    let status = match client.post_land_status().await {
        Ok(status) => status,
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::UnknownRequest => {
            return Ok(RosenBaseSnapshot::default());
        }
        Err(error) => return Err(error.into()),
    };
    let raw_failures = match client.post_land_failures(None).await {
        Ok(failures) => failures,
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::UnknownRequest => {
            Vec::new()
        }
        Err(error) => return Err(error.into()),
    };
    let platforms: Vec<RosenPlatformCheckpoint> = status
        .platforms
        .iter()
        .map(|entry| RosenPlatformCheckpoint {
            platform: entry.platform.clone(),
            display_name: rosen_core::Platform::display_label(&entry.platform),
            required_build: entry.required_build,
            required_test: entry.required_test,
            last_built: entry.last_built.as_ref().map(|cp| cp.commit.to_string()),
            last_green: entry.last_green.as_ref().map(|cp| cp.commit.to_string()),
            checker_connected: !entry.checkers.is_empty(),
        })
        .collect();
    let host = rosen_core::Platform::host();
    let mut failures = Vec::new();
    for entry in raw_failures {
        if owned_by_build_repairs(&entry, &host) {
            continue;
        }
        let commit = entry.commit.to_string();
        let checkpoint = checkpoint_for_failure(&status, &entry);
        let covered_by_green = match checkpoint {
            Some(green) => is_ancestor(&client, &commit, &green).await?,
            None => false,
        };
        let platform_display = entry
            .platform
            .as_deref()
            .map(rosen_core::Platform::display_label);
        failures.push(RosenPostLandFailure {
            session: u64::try_from(entry.session).unwrap_or(0),
            change: entry.change,
            commit,
            kind: entry.kind.as_str().to_owned(),
            platform: entry.platform,
            platform_display,
            output: entry.output,
            covered_by_green,
            reverted: entry.revert.is_some(),
        });
    }
    Ok(RosenBaseSnapshot {
        base: status.base.map(|commit| commit.to_string()),
        last_green: status.last_green.map(|cp| cp.commit.to_string()),
        platforms,
        failures,
        bisecting: !status.sessions.is_empty(),
    })
}

fn owned_by_build_repairs(
    failure: &rosen_client::protocol::PostLandFailure,
    host: &rosen_core::Platform,
) -> bool {
    let local_platform = failure.platform.as_deref().is_none_or(|raw| {
        rosen_core::Platform::parse(raw).is_ok_and(|target| host.matches(&target))
    });
    if !local_platform {
        return false;
    }
    let is_integration = failure.kind == CheckKind::Test
        && failure.revert.is_none()
        && failure.output.starts_with(integration::FAILURE_HEADING);
    is_integration || (failure.kind == CheckKind::Build && failure.requires_revert)
}

fn checkpoint_for_failure(
    status: &rosen_client::protocol::PostLandStatus,
    failure: &rosen_client::protocol::PostLandFailure,
) -> Option<String> {
    if let Some(platform) = failure.platform.as_deref()
        && let Some(entry) = status
            .platforms
            .iter()
            .find(|candidate| candidate.platform.eq_ignore_ascii_case(platform))
    {
        return match failure.kind {
            CheckKind::Test => entry.last_green.as_ref().map(|cp| cp.commit.to_string()),
            CheckKind::Build => entry
                .last_built
                .as_ref()
                .or(entry.last_green.as_ref())
                .map(|cp| cp.commit.to_string()),
        };
    }
    match failure.kind {
        CheckKind::Test => status.last_green.as_ref().map(|cp| cp.commit.to_string()),
        CheckKind::Build => status
            .last_built
            .as_ref()
            .or(status.last_green.as_ref())
            .map(|cp| cp.commit.to_string()),
    }
}

/// Reads a UTF-8 file from the project's Rosen base (or a specific commit) (#591).
pub async fn base_file_text(
    server: &ProjectServer,
    at: Option<&str>,
    path: &str,
) -> Result<Option<String>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let request = rosen_client::protocol::ReadRequest {
        at: at.map(str::to_owned),
        path: path.to_owned(),
    };
    match client.read_file(request).await {
        Ok(ReadAnswer::File { data, .. }) => Ok(Some(String::from_utf8_lossy(&data).into_owned())),
        Ok(ReadAnswer::Directory { .. }) => Ok(None),
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Lists recent commits on this project's Rosen base, newest first (#591).
pub async fn recent_base_commits(
    server: &ProjectServer,
    limit: u32,
) -> Result<Vec<velocity_core::rosen_pin::RosenCommit>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let log = client
        .log(LogRequest {
            at: None,
            paths: Vec::new(),
            limit: Some(limit),
        })
        .await?;
    Ok(log
        .entries
        .into_iter()
        .map(|entry| velocity_core::rosen_pin::RosenCommit {
            commit: entry.commit.to_string(),
            message: entry.summary,
        })
        .collect())
}
