//! A GitHub workflow run an agent requested (#508), owned by the session like a queued
//! command: the request is checked against the owner's allowlist before it is accepted,
//! Velocity dispatches and waits, and the result joins the agent's next prompt.

use std::time::Duration;

use agent_client_protocol::v1::schema::{
    SessionUpdate, ToolCall, ToolCallId, ToolCallStatus, ToolKind,
};
use tokio_util::sync::CancellationToken;
use velocity_core::text_budget::{PROMPT_BYTES, prompt_excerpt};
use velocity_core::workflow_dispatch::{RUN_TITLE, WorkflowRefusal, admit};
use velocity_github::{WorkflowDispatch, run_workflow};
use velocity_rosen::workflow_policy::{WorkflowPolicy, workflow_policy};

use super::AgentLaunch;
use super::queued_run_command::RunResult;
use super::workflow_run_tool::WorkflowRequest;

const RUN_ID_PREFIX: &str = "velocity-workflow-";
/// Tells the agent's `rosen` which server holds its project; the allowlist is read there.
const ROSEN_ENDPOINT: &str = "ROSEN_ENDPOINT";
/// The server is local and answers at once; the tool's own wait is a little longer.
const POLICY_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct WorkflowRun {
    id: ToolCallId,
    pub title: String,
    /// The GitHub repository the project mirrors to, whose workflow this is.
    mirror: String,
    dispatch: WorkflowDispatch,
}

impl WorkflowRun {
    /// The run to start, or the reason the request is refused. The allowlist comes from the
    /// `rosen.toml` in force at the base, which only a landed change can alter.
    pub async fn admit(request: WorkflowRequest, launch: &AgentLaunch) -> Result<Self, String> {
        let endpoint = launch
            .env
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(ROSEN_ENDPOINT))
            .map(|(_, endpoint)| endpoint.as_str())
            .ok_or(
                "Workflow runs need a Rosen workspace: the owner's allowlist is read from the \
                 project's rosen.toml, and this session has no Rosen server",
            )?;
        let policy = tokio::time::timeout(POLICY_TIMEOUT, workflow_policy(endpoint))
            .await
            .map_err(|_| "Rosen did not answer with the project's configuration".to_owned())?
            .map_err(|error| format!("Could not read the project's configuration: {error}"))?;
        let workflow = admit(
            &policy.dispatchable,
            &request.workflow,
            request.reference.as_deref(),
            &request.inputs,
        )
        .map_err(|refusal| refusal_reason(&refusal, &policy))?;
        let mirror = policy.mirror.ok_or(
            "The base's rosen.toml names no `[mirror] remote`, so there is no GitHub \
             repository to run the workflow in",
        )?;
        let title = match &request.reference {
            Some(reference) => format!("{RUN_TITLE}{workflow} on {reference}"),
            None => format!("{RUN_TITLE}{workflow}"),
        };
        Ok(Self {
            id: ToolCallId::new(format!("{RUN_ID_PREFIX}{}", uuid::Uuid::new_v4())),
            title,
            mirror,
            dispatch: WorkflowDispatch {
                workflow,
                reference: request.reference,
                inputs: request.inputs,
            },
        })
    }

    pub fn started(&self) -> SessionUpdate {
        SessionUpdate::ToolCall(
            ToolCall::new(self.id.clone(), self.title.clone())
                .kind(ToolKind::Execute)
                .status(ToolCallStatus::InProgress),
        )
    }

    /// Dispatches the workflow and waits for its run. Stopping the session ends the wait,
    /// not the run: cancelling half of a release is worse than letting it finish.
    pub async fn finish(self, stop: CancellationToken) -> RunResult {
        let (mirror, dispatch) = (self.mirror, self.dispatch);
        let stopped = stop.clone();
        // `gh` blocks, for hours on a long run, so the wait has a thread of its own.
        let watching = tokio::task::spawn_blocking(move || {
            run_workflow(&mirror, &dispatch, &|| stopped.is_cancelled())
        });
        let (succeeded, summary) = tokio::select! {
            outcome = watching => match outcome {
                Ok(outcome) => (outcome.succeeded, outcome.summary),
                Err(error) => (false, format!("{}: Velocity stopped watching it ({error})", self.title)),
            },
            // The watcher sees the same stop within a moment and ends by itself.
            () = stop.cancelled() => (false, format!("{}: the session stopped before the run finished", self.title)),
        };
        let result = prompt_excerpt(
            &format!("[Velocity] {summary}"),
            PROMPT_BYTES,
            "\n[Report excerpt. Read report.md in the folder named above for the rest.]",
        );
        RunResult::new(self.id, succeeded, result)
    }
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
