//! Classify launch intents after all keyboard, palette and composer routing.

use crate::build_gate::{BuildKind, LaunchKind, launch_block_reason};
use crate::layout_view::LayoutIntent;
use crate::navigation::NavigationIntent;

pub fn guard_intent(
    intent: LayoutIntent,
    project_spec_locked: Option<bool>,
    interview_thread: bool,
) -> Result<LayoutIntent, &'static str> {
    let reason = launch_kind(&intent, interview_thread)
        .and_then(|kind| launch_block_reason(kind, project_spec_locked));
    match reason {
        Some(reason) => Err(reason),
        None => Ok(intent),
    }
}

fn launch_kind(intent: &LayoutIntent, interview_thread: bool) -> Option<LaunchKind> {
    match intent {
        LayoutIntent::StartPrompt(_) => Some(LaunchKind::Build(BuildKind::Thread)),
        LayoutIntent::DispatchIssue(_) => Some(LaunchKind::Build(BuildKind::IssueDispatch)),
        LayoutIntent::SteerThread { .. } => Some(if interview_thread {
            LaunchKind::Interview
        } else {
            LaunchKind::Build(BuildKind::Thread)
        }),
        LayoutIntent::Navigation(intent) => navigation_launch_kind(*intent),
        LayoutIntent::SwitchProject(_)
        | LayoutIntent::SwarmIssues
        | LayoutIntent::OpenTask(_)
        | LayoutIntent::OpenPullRequest(_)
        | LayoutIntent::OpenIssue(_)
        | LayoutIntent::OpenWorktreeInExplorer(_)
        | LayoutIntent::SettingsTab(_)
        | LayoutIntent::Usage
        | LayoutIntent::Updates
        | LayoutIntent::AddTask(_)
        | LayoutIntent::ToggleProofListEntry(_) => None,
    }
}

fn navigation_launch_kind(intent: NavigationIntent) -> Option<LaunchKind> {
    use NavigationIntent::*;
    match intent {
        Fanout => Some(LaunchKind::Build(BuildKind::FanOut)),
        // These open views, pickers or configuration dialogs. Their eventual
        // launch commands must check require_launch afresh before doing I/O.
        Palette | NewThread | NewProject | Settings | ShowNumbers | Jump(_) | Search | Task
        | Swarm | Goal | Airc | Dismiss | Submit | Dispatch | Resume | PrDuty | Scouts
        | MainWatcher | Answer(_) | Lock | SpecFile | Grill | Diff | Editor | SaveAgents
        | Board | Focus => None,
    }
}
