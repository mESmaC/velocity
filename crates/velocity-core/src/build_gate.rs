//! Project spec eligibility for every build launch (SPEC.md Rule 2 and F3).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildKind {
    Thread,
    FanOut,
    TaskSwarm,
    IssueSwarm,
    GoalSwarm,
    IssueDispatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    Interview,
    Build(BuildKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchDecision {
    Interview,
    Build(BuildKind),
    LockSpecFirst,
}

/// Check the authoritative project snapshot before allocating a worktree or agent.
/// Interviews operate in the main checkout, so they do not require a locked spec.
pub fn launch_decision(kind: LaunchKind, spec_locked: bool) -> LaunchDecision {
    match kind {
        LaunchKind::Interview => LaunchDecision::Interview,
        LaunchKind::Build(build_kind) => {
            if spec_locked {
                LaunchDecision::Build(build_kind)
            } else {
                LaunchDecision::LockSpecFirst
            }
        }
    }
}

/// An absent active project cannot authorize an interview or a build launch.
pub fn project_launch_decision(
    kind: LaunchKind,
    active_project_spec_locked: Option<bool>,
) -> Option<LaunchDecision> {
    active_project_spec_locked.map(|spec_locked| launch_decision(kind, spec_locked))
}

pub fn launch_block_reason(
    kind: LaunchKind,
    active_project_spec_locked: Option<bool>,
) -> Option<&'static str> {
    match project_launch_decision(kind, active_project_spec_locked) {
        None => Some("Select or create a project first"),
        Some(LaunchDecision::LockSpecFirst) => Some("Lock the spec first"),
        Some(LaunchDecision::Interview | LaunchDecision::Build(_)) => None,
    }
}
