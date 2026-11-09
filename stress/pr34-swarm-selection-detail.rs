//! PR-duty reproducer for SPEC.md F10's selection-dependent sidebar members.
//! Compile against the reviewed velocity-core library; this uses its real reducer.
//! Adapted from stress/pr4-swarm-selection.rs for ThreadEntry.detail (#21); logic unchanged.

use std::process::ExitCode;
use velocity_core::layout::{LayoutRow, ThreadGroup};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::{LayoutAction, LayoutData, ThreadEntry};
use velocity_core::navigation::NavigationView;

fn entry(identifier: usize, group: ThreadGroup, parent: Option<usize>) -> ThreadEntry {
    ThreadEntry {
        policy: LayoutRow {
            identifier,
            group,
            hidden: false,
            matches_search: true,
            member_of: parent,
            expanded: false,
            agent_count: 0,
            paused: false,
        },
        title: format!("Thread {identifier}"),
        metadata: String::new(),
        view: NavigationView::Thread,
        detail: None,
    }
}

fn check(state: &LayoutState, expected: usize, context: &str) -> usize {
    let actual = state
        .snapshot()
        .groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.member)
        .count();
    if actual == expected {
        println!("PASS {context}: {actual} member rows");
        0
    } else {
        println!("FAIL {context}: expected {expected} member rows, got {actual}");
        1
    }
}

fn main() -> ExitCode {
    let mut state = LayoutState::default();
    state.replace_data(LayoutData {
        threads: vec![
            entry(10, ThreadGroup::Swarm, None),
            entry(11, ThreadGroup::Working, Some(10)),
            entry(20, ThreadGroup::Working, None),
        ],
        ..LayoutData::default()
    });
    let mut failures = check(&state, 0, "home has no selected swarm");
    state.apply(LayoutAction::OpenThread(10));
    failures += check(&state, 1, "selecting the swarm reveals its member");
    state.apply(LayoutAction::OpenThread(11));
    failures += check(&state, 1, "selecting a member reveals its siblings");
    state.apply(LayoutAction::ToggleSwarm(10));
    failures += check(&state, 1, "manual expansion control");
    state.apply(LayoutAction::OpenThread(20));
    failures += check(&state, 0, "selecting an unrelated thread hides members");
    println!("failures: {failures}");
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
