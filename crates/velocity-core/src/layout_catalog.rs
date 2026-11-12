//! Stable command catalog and the reference's labels.

use crate::layout::{PulseKind, ThreadGroup};
use crate::layout_view::{LayoutIntent, PaletteItem, PaletteTarget};
use crate::navigation::NavigationIntent;

pub const GROUPS: [ThreadGroup; 8] = [
    ThreadGroup::Draft,
    ThreadGroup::Duty,
    ThreadGroup::Swarm,
    ThreadGroup::Spec,
    ThreadGroup::Working,
    ThreadGroup::Review,
    ThreadGroup::Error,
    ThreadGroup::Settled,
];

pub const PULSE: [(PulseKind, &str); 6] = [
    (PulseKind::Duty, "on duty"),
    (PulseKind::Spec, "in spec"),
    (PulseKind::Running, "running"),
    (PulseKind::Watching, "PRs watched"),
    (PulseKind::Failed, "failed"),
    (PulseKind::MergedToday, "merged today"),
];

pub fn group_title(group: ThreadGroup) -> &'static str {
    match group {
        ThreadGroup::Draft => "",
        ThreadGroup::Duty => "On duty",
        ThreadGroup::Swarm => "Swarms",
        ThreadGroup::Spec => "Spec interview",
        ThreadGroup::Working => "Running",
        ThreadGroup::Review => "PR open · watching",
        ThreadGroup::Error => "Failed",
        ThreadGroup::Closed => "Closed · not merged",
        ThreadGroup::Done => "Done",
        ThreadGroup::Merged => "Merged",
        ThreadGroup::Settled => "Settled",
    }
}

enum CommandTarget {
    Navigation(NavigationIntent),
    Settings(&'static str),
    SwarmIssues,
}

const COMMANDS: [(&str, &str, CommandTarget); 16] = {
    use CommandTarget::{Navigation, Settings, SwarmIssues};
    use NavigationIntent::*;
    [
        ("New thread", "Ctrl N", Navigation(NewThread)),
        ("New project…", "Ctrl Shift N", Navigation(NewProject)),
        ("Edit AGENTS.md", "file", Settings("agents")),
        ("Add a task to the Task List", "T", Navigation(Task)),
        ("Settings · Worktrees", "Ctrl ,", Settings("worktrees")),
        ("Settings · Providers (sign in)", "", Settings("providers")),
        ("Settings · Proofs", "", Settings("proofs")),
        ("AIRC · agent chat", "A", Navigation(Airc)),
        (
            "Goal swarm · many agents, one prompt…",
            "G",
            Navigation(Goal),
        ),
        ("Swarm the Task List…", "W", Navigation(Swarm)),
        ("Swarm open issues…", "swarm", SwarmIssues),
        ("Dispatch issue to agent…", "I", Navigation(Dispatch)),
        ("Fan out prompt across 3 models", "F", Navigation(Fanout)),
        ("Put agents on PR duty…", "V", Navigation(PrDuty)),
        ("Send scouts to review the repo…", "S", Navigation(Scouts)),
        (
            "Put an agent on main watch…",
            "watch",
            Navigation(MainWatcher),
        ),
    ]
};

pub fn commands() -> Vec<PaletteItem> {
    COMMANDS
        .into_iter()
        .map(|(label, shortcut, target)| {
            let intent = match target {
                CommandTarget::Navigation(intent) => LayoutIntent::Navigation(intent),
                CommandTarget::Settings(tab) => LayoutIntent::SettingsTab(tab.into()),
                CommandTarget::SwarmIssues => LayoutIntent::SwarmIssues,
            };
            PaletteItem {
                identifier: format!("command:{label}"),
                label: label.into(),
                group: "command".into(),
                shortcut: shortcut.into(),
                target: PaletteTarget::Intent(intent),
            }
        })
        .collect()
}
