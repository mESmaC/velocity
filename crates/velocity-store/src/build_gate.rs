//! Adapt persisted active-project state to the launch selector and layout gate.

use serde::{Deserialize, Serialize};
use specta::Type;
use thiserror::Error;
use velocity_core::build_gate::{BuildKind, LaunchDecision, LaunchKind, project_launch_decision};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutIntent, LayoutSnapshot};

use crate::PersistedState;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LaunchRequest {
    Interview,
    Thread,
    FanOut,
    TaskSwarm,
    IssueSwarm,
    GoalSwarm,
    IssueDispatch,
}

impl From<LaunchRequest> for LaunchKind {
    fn from(request: LaunchRequest) -> Self {
        match request {
            LaunchRequest::Interview => Self::Interview,
            LaunchRequest::Thread => Self::Build(BuildKind::Thread),
            LaunchRequest::FanOut => Self::Build(BuildKind::FanOut),
            LaunchRequest::TaskSwarm => Self::Build(BuildKind::TaskSwarm),
            LaunchRequest::IssueSwarm => Self::Build(BuildKind::IssueSwarm),
            LaunchRequest::GoalSwarm => Self::Build(BuildKind::GoalSwarm),
            LaunchRequest::IssueDispatch => Self::Build(BuildKind::IssueDispatch),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LaunchEligibility {
    Allowed,
    LockSpecFirst,
    NoProject,
}

#[derive(Debug, Error)]
pub enum LaunchBlocked {
    #[error("Lock the spec first")]
    LockSpecFirst,
    #[error("Select or create a project first")]
    NoProject,
}

fn active_project_spec_locked(state: &PersistedState) -> Option<bool> {
    usize::try_from(state.active_project_index)
        .ok()
        .and_then(|index| state.projects.get(index))
        .map(|project| project.spec_locked)
}

/// UI queries are advisory; effectful launch handlers must call the guard afresh.
pub fn launch_eligibility(state: &PersistedState, request: LaunchRequest) -> LaunchEligibility {
    match project_launch_decision(request.into(), active_project_spec_locked(state)) {
        None => LaunchEligibility::NoProject,
        Some(LaunchDecision::LockSpecFirst) => LaunchEligibility::LockSpecFirst,
        Some(LaunchDecision::Interview | LaunchDecision::Build(_)) => LaunchEligibility::Allowed,
    }
}

pub fn layout_snapshot(state: &PersistedState, layout: &mut LayoutState) -> LayoutSnapshot {
    sync_layout(state, layout);
    layout.snapshot()
}

pub fn sync_layout(state: &PersistedState, layout: &mut LayoutState) {
    layout.set_project_spec_locked(active_project_spec_locked(state));
}

/// Both locks stay held while routing so a stale frontend cannot authorize work.
pub fn dispatch_layout(
    state: &PersistedState,
    layout: &mut LayoutState,
    action: LayoutAction,
) -> Option<LayoutIntent> {
    sync_layout(state, layout);
    layout.apply(action)
}

/// Call under the store lock before retaining the selected project for a launch.
pub fn require_launch(state: &PersistedState, request: LaunchRequest) -> Result<(), LaunchBlocked> {
    match launch_eligibility(state, request) {
        LaunchEligibility::Allowed => Ok(()),
        LaunchEligibility::LockSpecFirst => Err(LaunchBlocked::LockSpecFirst),
        LaunchEligibility::NoProject => Err(LaunchBlocked::NoProject),
    }
}
