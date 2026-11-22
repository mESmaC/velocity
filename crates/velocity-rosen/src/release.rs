//! The commit a release is cut from (#431, #591): Rosen R12's last green commit, or a
//! landed commit it covers, requiring every required platform green when the base
//! configures per-platform post-land checks (Rosen #440).

use rosen_client::Client;
use rosen_client::protocol::{ErrorCode, PlatformStatus, PostLandStatus};
use rosen_core::Platform;

use crate::{ProjectServer, RosenError};

/// The commit to release and the green commit that covers it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseCommit {
    pub commit: String,
    pub green: String,
}

/// One required platform that has not yet certified the release commit (#591).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitingPlatform {
    pub platform: String,
    pub display_name: String,
    pub checker_connected: bool,
}

/// Result of checking Rosen's per-platform release gate (#591).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReleaseGateCheck {
    Ready(ReleaseCommit),
    Waiting {
        commit: Option<String>,
        platforms: Vec<WaitingPlatform>,
        local_needs_run: bool,
        message: String,
    },
}

/// `requested` when Rosen's per-platform green checkpoints cover it, or the newest commit
/// green on every required platform when no commit is requested.
pub async fn release_commit(
    server: &ProjectServer,
    requested: Option<&str>,
) -> Result<ReleaseCommit, RosenError> {
    match check_release_gate(server, requested).await? {
        ReleaseGateCheck::Ready(release) => Ok(release),
        ReleaseGateCheck::Waiting { message, .. } => Err(RosenError::Release(message)),
    }
}

/// Checks Rosen's post-land status and every required platform for `requested` (or the
/// newest commit green across all required platforms when `requested` is `None`) (#591).
pub async fn check_release_gate(
    server: &ProjectServer,
    requested: Option<&str>,
) -> Result<ReleaseGateCheck, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let requested = requested.map(str::trim).filter(|text| !text.is_empty());
    let status = match client.post_land_status().await {
        Ok(status) => status,
        Err(rosen_client::Error::Server(error)) if error.code == ErrorCode::UnknownRequest => {
            return legacy_release_gate(&client, requested).await;
        }
        Err(error) => return Err(error.into()),
    };
    evaluate_status(&client, &status, requested).await
}

async fn legacy_release_gate(
    client: &Client,
    requested: Option<&str>,
) -> Result<ReleaseGateCheck, RosenError> {
    let Some(green) = client.post_land_next().await?.last_green else {
        return Ok(ReleaseGateCheck::Waiting {
            commit: requested.map(str::to_owned),
            platforms: Vec::new(),
            local_needs_run: true,
            message:
                "Rosen has no green commit yet: the post-land tests have not passed on any land"
                    .into(),
        });
    };
    let green_text = green.to_string();
    let Some(requested) = requested else {
        return Ok(ReleaseGateCheck::Ready(ReleaseCommit {
            commit: green_text.clone(),
            green: green_text,
        }));
    };
    let identifier = crate::policy::object(requested)?;
    if !covers(client, &identifier.to_string(), &green_text).await? {
        return Ok(ReleaseGateCheck::Waiting {
            commit: Some(identifier.to_string()),
            platforms: Vec::new(),
            local_needs_run: true,
            message: format!(
                "{requested} is not covered by Rosen's last green commit {green_text}. Release a landed commit the post-land tests have passed on."
            ),
        });
    }
    Ok(ReleaseGateCheck::Ready(ReleaseCommit {
        commit: identifier.to_string(),
        green: green_text,
    }))
}

async fn evaluate_status(
    client: &Client,
    status: &PostLandStatus,
    requested: Option<&str>,
) -> Result<ReleaseGateCheck, RosenError> {
    let required: Vec<&PlatformStatus> = status
        .platforms
        .iter()
        .filter(|platform| platform.required_test || platform.required_build)
        .collect();
    if required.is_empty() {
        return evaluate_single_prefix(client, status, requested).await;
    }
    match requested {
        Some(target) => evaluate_requested_platforms(client, status, &required, target).await,
        None => evaluate_latest_platforms(client, status, &required).await,
    }
}

async fn evaluate_single_prefix(
    client: &Client,
    status: &PostLandStatus,
    requested: Option<&str>,
) -> Result<ReleaseGateCheck, RosenError> {
    let Some(green) = status.last_green.as_ref().map(|cp| cp.commit.to_string()) else {
        return Ok(ReleaseGateCheck::Waiting {
            commit: requested.map(str::to_owned),
            platforms: Vec::new(),
            local_needs_run: true,
            message:
                "Rosen has no green commit yet: the post-land tests have not passed on any land"
                    .into(),
        });
    };
    let Some(target) = requested else {
        return Ok(ReleaseGateCheck::Ready(ReleaseCommit {
            commit: green.clone(),
            green,
        }));
    };
    let identifier = crate::policy::object(target)?;
    let target_text = identifier.to_string();
    if !covers(client, &target_text, &green).await? {
        return Ok(ReleaseGateCheck::Waiting {
            commit: Some(target_text),
            platforms: Vec::new(),
            local_needs_run: true,
            message: format!(
                "{target} is not covered by Rosen's last green commit {green}. Release a landed commit the post-land tests have passed on."
            ),
        });
    }
    Ok(ReleaseGateCheck::Ready(ReleaseCommit {
        commit: target_text,
        green,
    }))
}

async fn evaluate_requested_platforms(
    client: &Client,
    status: &PostLandStatus,
    required: &[&PlatformStatus],
    requested: &str,
) -> Result<ReleaseGateCheck, RosenError> {
    let identifier = crate::policy::object(requested)?;
    let target = identifier.to_string();
    let mut waiting = Vec::new();
    for platform in required {
        if !platform_covers(client, platform, &target).await? {
            waiting.push(to_waiting(platform));
        }
    }
    if !waiting.is_empty() {
        let local_needs_run = local_platform_waiting(&waiting);
        let message = format_waiting_message(Some(&target), &waiting);
        return Ok(ReleaseGateCheck::Waiting {
            commit: Some(target),
            platforms: waiting,
            local_needs_run,
            message,
        });
    }
    let green = match status.last_green.as_ref().map(|cp| cp.commit.to_string()) {
        Some(overall) if covers(client, &target, &overall).await? => overall,
        _ => target.clone(),
    };
    Ok(ReleaseGateCheck::Ready(ReleaseCommit {
        commit: target,
        green,
    }))
}

async fn evaluate_latest_platforms(
    client: &Client,
    status: &PostLandStatus,
    required: &[&PlatformStatus],
) -> Result<ReleaseGateCheck, RosenError> {
    let mut waiting = Vec::new();
    let mut checkpoints = Vec::new();
    for platform in required {
        match platform_effective_checkpoint(client, platform).await? {
            Some(commit) => checkpoints.push(commit),
            None => waiting.push(to_waiting(platform)),
        }
    }
    if !waiting.is_empty() {
        let local_needs_run = local_platform_waiting(&waiting);
        let base = status.base.map(|commit| commit.to_string());
        let message = format_waiting_message(base.as_deref(), &waiting);
        return Ok(ReleaseGateCheck::Waiting {
            commit: base,
            platforms: waiting,
            local_needs_run,
            message,
        });
    }
    let Some(green) = common_ancestor(client, &checkpoints).await? else {
        let base = status.base.map(|commit| commit.to_string());
        return Ok(ReleaseGateCheck::Waiting {
            commit: base,
            platforms: Vec::new(),
            local_needs_run: true,
            message: "Rosen has no commit covered by every required platform yet.".into(),
        });
    };
    Ok(ReleaseGateCheck::Ready(ReleaseCommit {
        commit: green.clone(),
        green,
    }))
}

async fn platform_covers(
    client: &Client,
    platform: &PlatformStatus,
    target: &str,
) -> Result<bool, RosenError> {
    if platform.required_test {
        let Some(green) = platform.last_green.as_ref() else {
            return Ok(false);
        };
        if !covers(client, target, &green.commit.to_string()).await? {
            return Ok(false);
        }
    }
    if platform.required_build {
        let built = platform
            .last_built
            .as_ref()
            .or(platform.last_green.as_ref());
        let Some(built) = built else {
            return Ok(false);
        };
        if !covers(client, target, &built.commit.to_string()).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn platform_effective_checkpoint(
    client: &Client,
    platform: &PlatformStatus,
) -> Result<Option<String>, RosenError> {
    let mut commits = Vec::new();
    if platform.required_test {
        let Some(green) = platform.last_green.as_ref() else {
            return Ok(None);
        };
        commits.push(green.commit.to_string());
    }
    if platform.required_build {
        let Some(built) = platform
            .last_built
            .as_ref()
            .or(platform.last_green.as_ref())
        else {
            return Ok(None);
        };
        commits.push(built.commit.to_string());
    }
    common_ancestor(client, &commits).await
}

async fn common_ancestor(
    client: &Client,
    commits: &[String],
) -> Result<Option<String>, RosenError> {
    let Some((first, rest)) = commits.split_first() else {
        return Ok(None);
    };
    let mut current = first.clone();
    for next in rest {
        if current.eq_ignore_ascii_case(next) {
            continue;
        }
        match client.merge_base(&current, next).await {
            Ok(answer) => match answer.merge_base {
                Some(base) => current = base.to_string(),
                None => return Ok(None),
            },
            Err(rosen_client::Error::Server(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(current))
}

async fn covers(client: &Client, target: &str, checkpoint: &str) -> Result<bool, RosenError> {
    if target.eq_ignore_ascii_case(checkpoint) {
        return Ok(true);
    }
    let Ok(identifier) = crate::policy::object(target) else {
        return Ok(false);
    };
    match client.merge_base(target, checkpoint).await {
        Ok(answer) => Ok(answer.merge_base == Some(identifier)),
        Err(rosen_client::Error::Server(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn to_waiting(platform: &PlatformStatus) -> WaitingPlatform {
    WaitingPlatform {
        platform: platform.platform.clone(),
        display_name: Platform::display_label(&platform.platform),
        checker_connected: !platform.checkers.is_empty(),
    }
}

fn local_platform_waiting(waiting: &[WaitingPlatform]) -> bool {
    let host = Platform::host();
    waiting.iter().any(|platform| {
        Platform::parse(&platform.platform).is_ok_and(|target| host.matches(&target))
    })
}

fn format_waiting_message(commit: Option<&str>, waiting: &[WaitingPlatform]) -> String {
    let names: Vec<String> = waiting
        .iter()
        .map(|platform| {
            if platform.checker_connected {
                platform.display_name.clone()
            } else {
                format!("{} (no checker connected)", platform.display_name)
            }
        })
        .collect();
    match commit {
        Some(commit) => {
            let short = commit.get(..10).unwrap_or(commit);
            format!(
                "Waiting for required platforms to pass post-land checks on {short}: {}",
                names.join(", ")
            )
        }
        None => format!(
            "Waiting for required platforms to pass post-land checks: {}",
            names.join(", ")
        ),
    }
}
