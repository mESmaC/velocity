//! A GitHub workflow run (#508) or Rosen check-set run (#591) an agent requested, owned by
//! the session like a queued command: the request is checked against the base `rosen.toml`
//! before it is accepted, Velocity runs or waits for it, and the result joins the agent's
//! next prompt.

use std::time::Duration;

use agent_client_protocol::v1::schema::{
    SessionUpdate, ToolCall, ToolCallId, ToolCallStatus, ToolKind,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use velocity_core::text_budget::{PROMPT_BYTES, prompt_excerpt};
use velocity_core::workflow_dispatch::{
    AdmittedRosenCheck, ConfiguredCheckSet, ROSEN_CHECK_TITLE, RUN_TITLE, RosenCheckKind,
    WorkflowRefusal, admit, admit_rosen_check_set,
};
use velocity_github::{WorkflowDispatch, run_workflow};
use velocity_rosen::check_run::run_check_set;
use velocity_rosen::workflow_policy::{WorkflowPolicy, workflow_policy};

use super::AgentLaunch;
use super::queued_run_command::RunResult;
use super::workflow_run_tool::WorkflowRequest;

const RUN_ID_PREFIX: &str = "velocity-workflow-";
/// Tells the agent's `rosen` which server holds its project; the allowlist is read there.
const ROSEN_ENDPOINT: &str = "ROSEN_ENDPOINT";
/// Injected by Velocity to indicate whether the project hosts issues on Collapse (#591).
const VELOCITY_ISSUE_HOST: &str = "VELOCITY_ISSUE_HOST";
/// The server is local and answers at once; the tool's own wait is a little longer.
const POLICY_TIMEOUT: Duration = Duration::from_secs(10);

enum RunTarget {
    GitHub {
        mirror: String,
        dispatch: WorkflowDispatch,
    },
    Rosen {
        endpoint: String,
        admitted: AdmittedRosenCheck,
    },
}

pub(super) struct WorkflowRun {
    id: ToolCallId,
    pub title: String,
    target: RunTarget,
}

impl WorkflowRun {
    /// The run to start, or the reason the request is refused. Policy comes from the
    /// `rosen.toml` in force at the base, which only a landed change can alter.
    pub async fn admit(request: WorkflowRequest, launch: &AgentLaunch) -> Result<Self, String> {
        let endpoint = launch
            .env
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(ROSEN_ENDPOINT))
            .map(|(_, endpoint)| endpoint.as_str())
            .ok_or(
                "Workflow and check-set runs need a Rosen workspace: policy is read from the \
                 project's rosen.toml, and this session has no Rosen server",
            )?;
        let policy = tokio::time::timeout(POLICY_TIMEOUT, workflow_policy(endpoint))
            .await
            .map_err(|_| "Rosen did not answer with the project's configuration".to_owned())?
            .map_err(|error| format!("Could not read the project's configuration: {error}"))?;
        if use_rosen_check_set(&request, &policy, launch) {
            return admit_rosen(request, endpoint, &policy);
        }
        admit_github(request, &policy)
    }

    pub fn started(&self) -> SessionUpdate {
        SessionUpdate::ToolCall(
            ToolCall::new(self.id.clone(), self.title.clone())
                .kind(ToolKind::Execute)
                .status(ToolCallStatus::InProgress),
        )
    }

    /// Runs or watches the requested check. Stopping the session ends the wait cleanly.
    pub async fn finish(self, stop: CancellationToken) -> RunResult {
        match self.target {
            RunTarget::GitHub { mirror, dispatch } => {
                finish_github(self.id, self.title, mirror, dispatch, stop).await
            }
            RunTarget::Rosen { endpoint, admitted } => {
                finish_rosen(self.id, endpoint, admitted, stop).await
            }
        }
    }
}

fn use_rosen_check_set(
    request: &WorkflowRequest,
    policy: &WorkflowPolicy,
    launch: &AgentLaunch,
) -> bool {
    if request.check_set.is_some() || request.platform.is_some() {
        return true;
    }
    if launch
        .env_value(VELOCITY_ISSUE_HOST)
        .is_some_and(|host| host.eq_ignore_ascii_case("collapse"))
    {
        return true;
    }
    let has_github_mirror = policy
        .mirror
        .as_deref()
        .and_then(velocity_core::collapse::github_repository_from_remote)
        .is_some();
    !has_github_mirror
}

fn admit_rosen(
    request: WorkflowRequest,
    endpoint: &str,
    policy: &WorkflowPolicy,
) -> Result<WorkflowRun, String> {
    let requested = request
        .check_set
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(request.workflow.as_str());
    let sets = configured_check_sets(policy);
    let host_os = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        _ => "linux",
    };
    let admitted = admit_rosen_check_set(
        &sets,
        requested,
        request.platform.as_deref(),
        request.reference.as_deref(),
        host_os,
    )
    .map_err(|error| error.to_string())?;
    let mut title = format!("{ROSEN_CHECK_TITLE}{}", admitted.check_set);
    if let Some(platform) = &admitted.platform {
        title.push_str(&format!(" ({platform})"));
    }
    if let Some(reference) = &admitted.reference {
        title.push_str(&format!(" at {reference}"));
    }
    Ok(WorkflowRun {
        id: ToolCallId::new(format!("{RUN_ID_PREFIX}{}", uuid::Uuid::new_v4())),
        title,
        target: RunTarget::Rosen {
            endpoint: endpoint.to_owned(),
            admitted,
        },
    })
}

fn configured_check_sets(policy: &WorkflowPolicy) -> Vec<ConfiguredCheckSet> {
    let pre = &policy.checks.pre_land;
    let post = &policy.checks.post_land;
    vec![
        ConfiguredCheckSet {
            kind: RosenCheckKind::PreLand,
            name: pre.name.clone(),
            platforms: pre
                .platforms
                .iter()
                .map(|p| p.as_str().to_owned())
                .collect(),
            has_commands: !pre.commands.is_empty() || !pre.platform_commands.is_empty(),
        },
        ConfiguredCheckSet {
            kind: RosenCheckKind::PostLand,
            name: post.name.clone(),
            platforms: post
                .platforms
                .iter()
                .map(|p| p.as_str().to_owned())
                .collect(),
            has_commands: !post.commands.is_empty() || !post.platform_commands.is_empty(),
        },
    ]
}

fn admit_github(request: WorkflowRequest, policy: &WorkflowPolicy) -> Result<WorkflowRun, String> {
    let workflow = admit(
        &policy.dispatchable,
        &request.workflow,
        request.reference.as_deref(),
        &request.inputs,
    )
    .map_err(|refusal| refusal_reason(&refusal, policy))?;
    let mirror = policy.mirror.clone().ok_or(
        "The base's rosen.toml names no `[mirror] remote`, so there is no GitHub \
         repository to run the workflow in",
    )?;
    let title = match &request.reference {
        Some(reference) => format!("{RUN_TITLE}{workflow} on {reference}"),
        None => format!("{RUN_TITLE}{workflow}"),
    };
    Ok(WorkflowRun {
        id: ToolCallId::new(format!("{RUN_ID_PREFIX}{}", uuid::Uuid::new_v4())),
        title,
        target: RunTarget::GitHub {
            mirror,
            dispatch: WorkflowDispatch {
                workflow,
                reference: request.reference,
                inputs: request.inputs,
            },
        },
    })
}

async fn finish_github(
    id: ToolCallId,
    title: String,
    mirror: String,
    dispatch: WorkflowDispatch,
    stop: CancellationToken,
) -> RunResult {
    let stopped = stop.clone();
    // `gh` blocks, for hours on a long run, so the wait has a thread of its own.
    let watching = tokio::task::spawn_blocking(move || {
        run_workflow(&mirror, &dispatch, &|| stopped.is_cancelled())
    });
    let (succeeded, summary) = tokio::select! {
        outcome = watching => match outcome {
            Ok(outcome) => (outcome.succeeded, outcome.summary),
            Err(error) => (false, format!("{title}: Velocity stopped watching it ({error})")),
        },
        () = stop.cancelled() => (false, format!("{title}: the session stopped before the run finished")),
    };
    let result = prompt_excerpt(
        &format!("[Velocity] {summary}"),
        PROMPT_BYTES,
        "\n[Report excerpt. Read report.md in the folder named above for the rest.]",
    );
    RunResult::new(id, succeeded, result)
}

async fn finish_rosen(
    id: ToolCallId,
    endpoint: String,
    admitted: AdmittedRosenCheck,
    stop: CancellationToken,
) -> RunResult {
    let (cancel_tx, cancel_rx) = watch::channel(false);
    let forward = stop.clone();
    tokio::spawn(async move {
        forward.cancelled().await;
        let _ = cancel_tx.send(true);
    });
    let outcome = run_check_set(&endpoint, &admitted, cancel_rx).await;
    let result = prompt_excerpt(
        &format!("[Velocity] {}", outcome.summary),
        PROMPT_BYTES,
        "\n[Output excerpt. Read the full log named above for the rest.]",
    );
    RunResult::new(id, outcome.succeeded, result)
}

/// The refusal as the agent reads it. A list that looks empty because Rosen refused the
/// base's file says so, since the owner may have added the workflow in that very file.
fn refusal_reason(refusal: &WorkflowRefusal, policy: &WorkflowPolicy) -> String {
    match (&policy.refused, refusal) {
        (Some(error), WorkflowRefusal::NotDispatchable { .. }) => format!(
            "{refusal} Rosen refuses the base's current rosen.toml ({error}), so the last \
             valid one is in force."
        ),
        _ => refusal.to_string(),
    }
}
