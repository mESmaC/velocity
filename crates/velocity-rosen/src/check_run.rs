//! Run or await a named Rosen check set on a platform for the `workflow_run` agent tool
//! on projects without GitHub Actions (#591).

use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rosen_client::protocol::{
    CheckKind, LogRequest, PlatformStatus, PostLandFailure, PostLandStatus,
};
use rosen_client::{Client, Endpoint};
use rosen_core::Platform;
use tokio::sync::watch;
use velocity_core::workflow_dispatch::{AdmittedRosenCheck, RosenCheckKind};

use crate::{RosenError, checker};

const REMOTE_WAIT_LIMIT: Duration = Duration::from_secs(6 * 60 * 60);
const REMOTE_POLL: Duration = Duration::from_secs(5);
const EXCERPT_CHARS: usize = 6_000;

/// Result delivered back to the requesting agent turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosenCheckOutcome {
    pub succeeded: bool,
    pub summary: String,
}

/// Executes a named Rosen check set locally when `platform` matches this machine, or
/// waits for Rosen's per-platform post-land checker when `platform` is remote (#591).
pub async fn run_check_set(
    endpoint: &str,
    admitted: &AdmittedRosenCheck,
    mut stop: watch::Receiver<bool>,
) -> RosenCheckOutcome {
    match execute(endpoint, admitted, &mut stop).await {
        Ok(outcome) => outcome,
        Err(error) => RosenCheckOutcome {
            succeeded: false,
            summary: format!(
                "Rosen check `{}` failed to run: {error}",
                admitted.check_set
            ),
        },
    }
}

async fn execute(
    endpoint: &str,
    admitted: &AdmittedRosenCheck,
    stop: &mut watch::Receiver<bool>,
) -> Result<RosenCheckOutcome, RosenError> {
    let client = crate::connection::requests(&Endpoint::new(endpoint)).await?;
    let commit = resolve_target_commit(&client, admitted.reference.as_deref()).await?;
    let config = client.project_config(Some(commit)).await?.config;
    let check_set = match admitted.kind {
        RosenCheckKind::PreLand => &config.checks.pre_land,
        RosenCheckKind::PostLand => &config.checks.post_land,
    };
    let target = admitted
        .platform
        .as_deref()
        .map(Platform::parse)
        .transpose()
        .map_err(RosenError::Release)?;
    let host = Platform::host();
    let run_locally = target
        .as_ref()
        .is_none_or(|platform| host.matches(platform));
    if run_locally {
        let commands = check_set.commands_for(target.as_ref().or(Some(&host)));
        return run_local_check(
            &client,
            commit,
            &admitted.check_set,
            target.as_ref().unwrap_or(&host),
            &commands,
        )
        .await;
    }
    let Some(remote_platform) = target else {
        return Err(RosenError::Release("Missing target platform".into()));
    };
    wait_for_remote_platform(&client, commit, admitted, &remote_platform, stop).await
}

async fn resolve_target_commit(
    client: &Client,
    reference: Option<&str>,
) -> Result<rosen_core::ObjectId, RosenError> {
    let log = client
        .log(LogRequest {
            at: reference.map(str::to_owned),
            paths: Vec::new(),
            limit: Some(1),
        })
        .await?;
    log.entries
        .first()
        .map(|entry| entry.commit)
        .ok_or_else(|| {
            RosenError::Release(match reference {
                Some(name) => format!("Rosen found no commit for `{name}`"),
                None => "Rosen base has no commits yet".into(),
            })
        })
}

async fn run_local_check(
    client: &Client,
    commit: rosen_core::ObjectId,
    check_set: &str,
    platform: &Platform,
    commands: &[String],
) -> Result<RosenCheckOutcome, RosenError> {
    let commit_hex = commit.to_string();
    let short = commit_hex.get(..10).unwrap_or(&commit_hex);
    if commands.is_empty() {
        return Ok(RosenCheckOutcome {
            succeeded: true,
            summary: format!(
                "Rosen check `{check_set}` on {} at {short} has no commands to run (passed).",
                platform.display_name()
            ),
        });
    }
    let work = temp_run_dir("checkout");
    let log_dir = temp_run_dir("log");
    let _ = std::fs::create_dir_all(&work);
    client.materialise_commit(commit, &work).await?;
    let commands_owned = commands.to_vec();
    let work_dir = work.clone();
    let started = Instant::now();
    let outcome = tokio::task::spawn_blocking(move || {
        checker::run_commands(&work_dir, &commands_owned, checker::Priority::Background)
    })
    .await
    .map_err(|error| RosenError::Release(error.to_string()))?;
    let _ = std::fs::remove_dir_all(&work);
    let elapsed = started.elapsed().as_secs();
    match outcome? {
        None => Ok(RosenCheckOutcome {
            succeeded: true,
            summary: format!(
                "Rosen check `{check_set}` passed on {} at {short} ({elapsed}s, {} commands).",
                platform.display_name(),
                commands.len()
            ),
        }),
        Some(failed) => {
            let log_path = save_output(&log_dir, &failed.output);
            let excerpt = tail_excerpt(&failed.output, EXCERPT_CHARS);
            let exit = failed
                .exit_code
                .map_or_else(|| "terminated".to_owned(), |code| format!("exit {code}"));
            let saved = log_path
                .as_ref()
                .map(|path| format!("\nFull output: {}", path.display()))
                .unwrap_or_default();
            Ok(RosenCheckOutcome {
                succeeded: false,
                summary: format!(
                    "Rosen check `{check_set}` failed on {} at {short} ({elapsed}s).\n\
                     Command: `{}` ({exit}){saved}\n\n{excerpt}",
                    platform.display_name(),
                    failed.command
                ),
            })
        }
    }
}

async fn wait_for_remote_platform(
    client: &Client,
    commit: rosen_core::ObjectId,
    admitted: &AdmittedRosenCheck,
    platform: &Platform,
    stop: &mut watch::Receiver<bool>,
) -> Result<RosenCheckOutcome, RosenError> {
    let commit_hex = commit.to_string();
    let check_kind = match admitted.kind {
        RosenCheckKind::PreLand => CheckKind::Build,
        RosenCheckKind::PostLand => CheckKind::Test,
    };
    let started = Instant::now();
    let mut initial_check = true;
    loop {
        if *stop.borrow() {
            return Ok(RosenCheckOutcome {
                succeeded: false,
                summary: format!(
                    "Rosen check `{}` on {}: the session stopped before the checker finished.",
                    admitted.check_set,
                    platform.display_name()
                ),
            });
        }
        let status = client.post_land_status().await?;
        let failures = client.post_land_failures(None).await.unwrap_or_default();
        if let Some(outcome) = evaluate_remote_state(
            client,
            &status,
            &failures,
            &commit_hex,
            admitted,
            check_kind,
            platform,
        )
        .await?
        {
            return Ok(outcome);
        }
        if initial_check {
            initial_check = false;
            if !has_connected_checker(&status, platform) {
                return Ok(RosenCheckOutcome {
                    succeeded: false,
                    summary: format!(
                        "No Rosen checker for {} is currently connected. Start `rosen checker run --platform {}` on an enrolled machine and try again.",
                        platform.display_name(),
                        platform.as_str()
                    ),
                });
            }
        }
        if started.elapsed() >= REMOTE_WAIT_LIMIT {
            return Ok(RosenCheckOutcome {
                succeeded: false,
                summary: format!(
                    "Timed out waiting for Rosen checker on {} to verify `{}` at {}.",
                    platform.display_name(),
                    admitted.check_set,
                    commit_hex.get(..10).unwrap_or(&commit_hex)
                ),
            });
        }
        tokio::select! {
            _ = stop.changed() => {}
            () = tokio::time::sleep(REMOTE_POLL) => {}
        }
    }
}

async fn evaluate_remote_state(
    client: &Client,
    status: &PostLandStatus,
    failures: &[PostLandFailure],
    commit: &str,
    admitted: &AdmittedRosenCheck,
    kind: CheckKind,
    platform: &Platform,
) -> Result<Option<RosenCheckOutcome>, RosenError> {
    let short = commit.get(..10).unwrap_or(commit);
    if let Some(entry) = find_platform_status(status, platform) {
        let checkpoint = match kind {
            CheckKind::Build => entry.last_built.as_ref().or(entry.last_green.as_ref()),
            CheckKind::Test => entry.last_green.as_ref(),
        };
        if let Some(cp) = checkpoint
            && is_covered(client, commit, &cp.commit.to_string()).await?
        {
            let by = cp
                .submitter
                .as_deref()
                .map(|who| format!(" by {who}"))
                .unwrap_or_default();
            return Ok(Some(RosenCheckOutcome {
                succeeded: true,
                summary: format!(
                    "Rosen check `{}` passed on {} at {short} (covered by {}{by}).",
                    admitted.check_set,
                    platform.display_name(),
                    cp.commit.to_string().get(..10).unwrap_or_default()
                ),
            }));
        }
    }
    let matching_failure = failures
        .iter()
        .filter(|failure| {
            failure.kind == kind
                && failure.revert.is_none()
                && failure
                    .platform
                    .as_deref()
                    .and_then(|raw| Platform::parse(raw).ok())
                    .is_some_and(|recorded| {
                        recorded.matches(platform) || platform.matches(&recorded)
                    })
        })
        .max_by_key(|failure| failure.session);
    if let Some(failure) = matching_failure
        && is_covered(client, &failure.commit.to_string(), commit).await?
    {
        let log_dir = temp_run_dir("remote-log");
        let log_path = save_output(&log_dir, &failure.output);
        let excerpt = tail_excerpt(&failure.output, EXCERPT_CHARS);
        let saved = log_path
            .as_ref()
            .map(|path| format!("\nFull output: {}", path.display()))
            .unwrap_or_default();
        return Ok(Some(RosenCheckOutcome {
            succeeded: false,
            summary: format!(
                "Rosen check `{}` failed on {} (covering {short}).\n\
                 Culprit change: {}\nCulprit commit: {}{saved}\n\n{excerpt}",
                admitted.check_set,
                platform.display_name(),
                failure.change,
                failure.commit
            ),
        }));
    }
    Ok(None)
}

fn find_platform_status<'a>(
    status: &'a PostLandStatus,
    target: &Platform,
) -> Option<&'a PlatformStatus> {
    status.platforms.iter().find(|entry| {
        Platform::parse(&entry.platform)
            .is_ok_and(|configured| target.matches(&configured) || configured.matches(target))
    })
}

fn has_connected_checker(status: &PostLandStatus, target: &Platform) -> bool {
    find_platform_status(status, target).is_some_and(|entry| !entry.checkers.is_empty())
}

async fn is_covered(client: &Client, ancestor: &str, descendant: &str) -> Result<bool, RosenError> {
    if ancestor.eq_ignore_ascii_case(descendant) {
        return Ok(true);
    }
    let Ok(identifier) = crate::policy::object(ancestor) else {
        return Ok(false);
    };
    match client.merge_base(ancestor, descendant).await {
        Ok(answer) => Ok(answer.merge_base == Some(identifier)),
        Err(rosen_client::Error::Server(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn temp_run_dir(tag: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "velocity-rosen-check-{tag}-{}-{unique}",
        std::process::id()
    ))
}

fn save_output(dir: &PathBuf, output: &str) -> Option<PathBuf> {
    if output.trim().is_empty() {
        return None;
    }
    std::fs::create_dir_all(dir).ok()?;
    let path = dir.join("output.log");
    std::fs::write(&path, output).ok()?;
    Some(path)
}

fn tail_excerpt(output: &str, limit: usize) -> &str {
    if output.len() <= limit {
        return output;
    }
    let start = output.ceil_char_boundary(output.len().saturating_sub(limit));
    &output[start..]
}
