//! Duty role domain types, rules, agents, activity feed, and state transitions (`SPEC.md §4 F7`, `§7 M5`).

use crate::layout_view::ProviderColor;
use crate::thread_view_model::ModelPillView;

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DutyRole {
    #[default]
    PrDuty,
    Scouts,
    /// Watches the base branch after merges (F7 Main watcher; the role name is proposed, Q22).
    MainWatcher,
}

impl DutyRole {
    pub fn title(self) -> &'static str {
        match self {
            Self::PrDuty => "PR duty",
            Self::Scouts => "Scouts",
            Self::MainWatcher => "Main watcher",
        }
    }

    pub fn prefix(self) -> &'static str {
        match self {
            Self::PrDuty => "pr",
            Self::Scouts => "scout",
            Self::MainWatcher => "watch",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::PrDuty => {
                "Watches open PRs: reviews the intent and every theorem statement, checks CI and proofs, and merges only what it approves."
            }
            Self::Scouts => {
                "Assumes every part of the app is broken and sets out to prove it: writes the tests and stress scripts, and files each issue with the failing one attached."
            }
            Self::MainWatcher => {
                "Watches master after every merge: runs every test and stress script, bisects a break to the merge that caused it, and opens a fix or revert PR."
            }
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Self::PrDuty => {
                "With several agents, each one claims a PR so two never review the same one."
            }
            Self::Scouts => "With several scouts, each takes a different area of the repo.",
            Self::MainWatcher => {
                "One watcher takes each merge. With several, a merge that lands mid-watch goes to the next one."
            }
        }
    }
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DutyMergeMethod {
    #[default]
    Squash,
    Rebase,
    MergeCommit,
}

impl DutyMergeMethod {
    pub fn rule_label(self) -> &'static str {
        match self {
            Self::Squash => "Squash merge",
            Self::Rebase => "Rebase merge",
            Self::MergeCommit => "Merge commit",
        }
    }
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoutFocusArea {
    Bugs,
    Stress,
    ProofGaps,
    SpecDrift,
    Performance,
    Security,
    DeadCode,
    Ux,
}

impl ScoutFocusArea {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bugs => "Bugs",
            Self::Stress => "Stress",
            Self::ProofGaps => "Proof gaps",
            Self::SpecDrift => "Spec drift",
            Self::Performance => "Performance",
            Self::Security => "Security",
            Self::DeadCode => "Dead code",
            Self::Ux => "UX",
        }
    }

    pub fn summary_token(self) -> &'static str {
        match self {
            Self::Bugs => "bugs",
            Self::Stress => "stress",
            Self::ProofGaps => "proof gaps",
            Self::SpecDrift => "spec drift",
            Self::Performance => "performance",
            Self::Security => "security",
            Self::DeadCode => "dead code",
            Self::Ux => "UX",
        }
    }
}

pub fn default_scout_focus_areas() -> Vec<ScoutFocusArea> {
    vec![
        ScoutFocusArea::Bugs,
        ScoutFocusArea::Stress,
        ScoutFocusArea::ProofGaps,
        ScoutFocusArea::SpecDrift,
    ]
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoutCadence {
    Once,
    #[default]
    EveryThirtyMinutes,
    AfterEveryMerge,
}

impl ScoutCadence {
    pub fn header_label(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::EveryThirtyMinutes => "every 30m",
            Self::AfterEveryMerge => "after every merge",
        }
    }

    pub fn rule_label(self) -> &'static str {
        match self {
            Self::Once => "Sweep once",
            Self::EveryThirtyMinutes => "Sweep every 30m",
            Self::AfterEveryMerge => "Sweep after every merge",
        }
    }
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrDutyRuleSettings {
    pub review_theorem_statements: bool,
    pub ci_green_before_merge: bool,
    pub check_diff_against_spec: bool,
    pub re_review_on_new_commits: bool,
    pub delete_branch_after_merge: bool,
    pub merge_method: DutyMergeMethod,
}

impl Default for PrDutyRuleSettings {
    fn default() -> Self {
        Self {
            review_theorem_statements: true,
            ci_green_before_merge: true,
            check_diff_against_spec: true,
            re_review_on_new_commits: true,
            delete_branch_after_merge: true,
            merge_method: DutyMergeMethod::Squash,
        }
    }
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScoutRuleSettings {
    pub focus_areas: Vec<ScoutFocusArea>,
    pub assume_every_part_fails: bool,
    pub run_stress_against_models: bool,
    pub label_issues_for_dispatch: bool,
    pub cadence: ScoutCadence,
}

impl Default for ScoutRuleSettings {
    fn default() -> Self {
        Self {
            focus_areas: default_scout_focus_areas(),
            assume_every_part_fails: true,
            run_stress_against_models: true,
            label_issues_for_dispatch: false,
            cadence: ScoutCadence::EveryThirtyMinutes,
        }
    }
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DutyRuleChip {
    pub text: String,
    pub hard: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DutyAgentCard {
    pub nickname: String,
    pub provider: ProviderColor,
    pub model_name: String,
    pub busy: bool,
    pub doing: String,
    /// A scout's last sweep. Saved state only; the window never reads it.
    #[cfg_attr(feature = "bindings", serde(skip))]
    #[cfg_attr(feature = "bindings", specta(skip))]
    pub sweep: Option<crate::scout_sweep::SweepRecord>,
    /// A PR duty agent's kept `review-pr-<n>` worktrees. Saved state only, so Settings →
    /// Worktrees lists them as PR duty's.
    #[cfg_attr(feature = "bindings", serde(skip))]
    #[cfg_attr(feature = "bindings", specta(skip))]
    pub review_worktrees: Vec<String>,
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DutyEventKind {
    Merge,
    Approve,
    Changes,
    Wait,
    File,
    Sweep,
    Claim,
    Skip,
    Task,
    You,
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DutyFeedEvent {
    pub timestamp: String,
    pub actor: String,
    pub kind: DutyEventKind,
    pub message: String,
}

impl DutyFeedEvent {
    pub fn new(timestamp: &str, actor: &str, kind: DutyEventKind, message: String) -> Self {
        Self {
            timestamp: timestamp.to_owned(),
            actor: actor.to_owned(),
            kind,
            message,
        }
    }
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct DutyViewSnapshot {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub role: DutyRole,
    pub title: String,
    pub paused: bool,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub agent_count: usize,
    pub agent_count_label: String,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub open_pull_request_count: usize,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub merged_today: usize,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub issues_filed: usize,
    pub cadence_label: String,
    pub rules: Vec<DutyRuleChip>,
    pub agents: Vec<DutyAgentCard>,
    pub feed: Vec<DutyFeedEvent>,
    pub composer_placeholder: String,
    pub model_pill: ModelPillView,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DutyRoleEntry {
    pub identifier: usize,
    pub role: DutyRole,
    pub paused: bool,
    pub issues_filed: usize,
    pub pr_rules: PrDutyRuleSettings,
    pub scout_rules: ScoutRuleSettings,
    pub agents: Vec<DutyAgentCard>,
    pub feed: Vec<DutyFeedEvent>,
}

pub fn clamp_duty_agent_count(count: usize) -> usize {
    count.clamp(1, 4)
}

pub fn build_pr_duty_rule_chips(settings: &PrDutyRuleSettings) -> Vec<DutyRuleChip> {
    let mut chips = vec![
        DutyRuleChip {
            text: "Never merge without an approving review".into(),
            hard: true,
        },
        DutyRuleChip {
            text: "Proofs check".into(),
            hard: true,
        },
    ];
    if settings.review_theorem_statements {
        chips.push(DutyRuleChip {
            text: "Review statements".into(),
            hard: false,
        });
    }
    if settings.ci_green_before_merge {
        chips.push(DutyRuleChip {
            text: "Local checks pass".into(),
            hard: false,
        });
    }
    if settings.check_diff_against_spec {
        chips.push(DutyRuleChip {
            text: "Check against SPEC.md".into(),
            hard: false,
        });
    }
    if settings.re_review_on_new_commits {
        chips.push(DutyRuleChip {
            text: "Re-review on new commits".into(),
            hard: false,
        });
    }
    chips.push(DutyRuleChip {
        text: settings.merge_method.rule_label().into(),
        hard: false,
    });
    if settings.delete_branch_after_merge {
        chips.push(DutyRuleChip {
            text: "Delete branch".into(),
            hard: false,
        });
    }
    chips
}

pub fn build_scout_rule_chips(settings: &ScoutRuleSettings) -> Vec<DutyRuleChip> {
    let mut chips = vec![
        DutyRuleChip {
            text: "Never file duplicates".into(),
            hard: true,
        },
        DutyRuleChip {
            text: "Evidence attached".into(),
            hard: true,
        },
    ];
    if settings.assume_every_part_fails {
        chips.push(DutyRuleChip {
            text: "Assume it fails".into(),
            hard: false,
        });
    }
    if let Some(focus) = format_focus_areas(&settings.focus_areas) {
        chips.push(DutyRuleChip {
            text: focus,
            hard: false,
        });
    }
    if settings.run_stress_against_models {
        chips.push(DutyRuleChip {
            text: "Diff against Lean models".into(),
            hard: false,
        });
    }
    if settings.label_issues_for_dispatch {
        chips.push(DutyRuleChip {
            text: "Label for dispatch".into(),
            hard: false,
        });
    }
    chips.push(DutyRuleChip {
        text: settings.cadence.rule_label().into(),
        hard: false,
    });
    chips
}

/// The Main watcher's rules (F7). It has no settings: what it runs comes from AGENTS.md.
pub fn build_main_watcher_rule_chips() -> Vec<DutyRuleChip> {
    let rule = |text: &str, hard: bool| DutyRuleChip {
        text: text.into(),
        hard,
    };
    vec![
        rule("Never push to master · every fix or revert is a PR", true),
        rule("Every test and stress script after each merge", false),
        rule("Bisect a break to its merge", false),
        rule("File flaky tests", false),
    ]
}

fn format_focus_areas(areas: &[ScoutFocusArea]) -> Option<String> {
    let first = areas.first()?;
    let mut text = String::from(first.label());
    for area in &areas[1..] {
        text.push_str(" · ");
        text.push_str(area.summary_token());
    }
    Some(text)
}

pub fn duty_sidebar_summary(entry: &DutyRoleEntry, merged_today: usize) -> String {
    if entry.paused {
        return "paused".into();
    }
    match entry.role {
        DutyRole::PrDuty => {
            let busy = entry.agents.iter().filter(|agent| agent.busy).count();
            format!("{busy} reviewing · {merged_today} merged")
        }
        DutyRole::Scouts => {
            let doing = entry
                .agents
                .first()
                .map_or("idle", |agent| agent.doing.as_str());
            format!("{} filed · {doing}", entry.issues_filed)
        }
        // The watcher on a merge right now, else what the first one says it waits for.
        DutyRole::MainWatcher => entry
            .agents
            .iter()
            .find(|agent| agent.busy)
            .or(entry.agents.first())
            .map_or_else(|| "idle".into(), |agent| agent.doing.clone()),
    }
}

pub fn initial_agent_doing(role: DutyRole, _has_open_pull_requests: bool) -> String {
    match role {
        // Open PRs describe available work, not whether a runner has started.
        // Only the runners can report claiming, watching or sweeping.
        DutyRole::PrDuty | DutyRole::Scouts => "waiting to start".into(),
        // The watch runner replaces this as soon as a watcher takes the base branch's tip.
        DutyRole::MainWatcher => "waiting for the first watch".into(),
    }
}

pub fn project_duty_view(
    entry: &DutyRoleEntry,
    open_pull_request_count: usize,
    merged_today: usize,
    default_pill: &ModelPillView,
) -> DutyViewSnapshot {
    let count = entry.agents.len();
    let agent_count_label = if count == 1 {
        "1 agent".into()
    } else {
        format!("{count} agents")
    };
    let rules = match entry.role {
        DutyRole::PrDuty => build_pr_duty_rule_chips(&entry.pr_rules),
        DutyRole::Scouts => build_scout_rule_chips(&entry.scout_rules),
        DutyRole::MainWatcher => build_main_watcher_rule_chips(),
    };
    let placeholder = match entry.role {
        DutyRole::PrDuty => "Instruct every PR agent, e.g. “hold merges until #41 lands”".into(),
        DutyRole::Scouts => "Point the scouts somewhere, e.g. “dig into the IPC layer”".into(),
        DutyRole::MainWatcher => {
            "Instruct the watcher, e.g. “run the stress suite on master again”".into()
        }
    };
    let model_pill = entry.agents.first().map_or_else(
        || default_pill.clone(),
        |agent| ModelPillView {
            provider: agent.provider,
            model_name: agent.model_name.clone(),
            effort_label: default_pill.effort_label.clone(),
            show_fast_badge: default_pill.show_fast_badge,
            speed_control_enabled: default_pill.speed_control_enabled,
            preserve_requested_fast: default_pill.preserve_requested_fast,
        },
    );
    DutyViewSnapshot {
        identifier: entry.identifier,
        role: entry.role,
        title: entry.role.title().into(),
        paused: entry.paused,
        agent_count: count,
        agent_count_label,
        open_pull_request_count,
        merged_today,
        issues_filed: entry.issues_filed,
        cadence_label: entry.scout_rules.cadence.header_label().into(),
        rules,
        agents: entry.agents.clone(),
        feed: entry.feed.clone(),
        composer_placeholder: placeholder,
        model_pill,
    }
}

pub struct DutyStartSpec {
    pub role: DutyRole,
    pub agent_count: usize,
    pub provider: ProviderColor,
    pub model_name: String,
    pub pr_rules: PrDutyRuleSettings,
    pub scout_rules: ScoutRuleSettings,
    pub has_open_pull_requests: bool,
    pub timestamp: String,
}

/// New agents take the lowest numbers no agent on the role holds, so a nick freed by a
/// dismissal is reused and two agents never share one (F7 Dismiss).
pub fn build_duty_agents(
    role: DutyRole,
    existing: &[DutyAgentCard],
    count: usize,
    provider: ProviderColor,
    model_name: &str,
    has_open_pull_requests: bool,
) -> Vec<DutyAgentCard> {
    let clamped = clamp_duty_agent_count(count);
    let doing = initial_agent_doing(role, has_open_pull_requests);
    let taken = |nickname: &str| existing.iter().any(|agent| agent.nickname == nickname);
    (1..)
        .map(|number: usize| format!("{}-{number}", role.prefix()))
        .filter(|nickname| !taken(nickname))
        .take(clamped)
        .map(|nickname| DutyAgentCard {
            nickname,
            provider,
            model_name: model_name.to_owned(),
            busy: false,
            doing: doing.clone(),
            sweep: None,
            review_worktrees: Vec::new(),
        })
        .collect()
}

pub fn start_duty_entry(identifier: usize, spec: DutyStartSpec) -> (DutyRoleEntry, DutyFeedEvent) {
    let clamped = clamp_duty_agent_count(spec.agent_count);
    let agents = build_duty_agents(
        spec.role,
        &[],
        clamped,
        spec.provider,
        &spec.model_name,
        spec.has_open_pull_requests,
    );
    let suffix = if clamped == 1 { "" } else { "s" };
    let role_lower = spec.role.title().to_lowercase();
    let event = DutyFeedEvent {
        timestamp: spec.timestamp,
        actor: "you".into(),
        kind: DutyEventKind::You,
        message: format!("Started {role_lower} with {clamped} agent{suffix}"),
    };
    let entry = DutyRoleEntry {
        identifier,
        role: spec.role,
        paused: false,
        issues_filed: 0,
        pr_rules: spec.pr_rules,
        scout_rules: spec.scout_rules,
        agents,
        feed: vec![event.clone()],
    };
    (entry, event)
}

pub fn add_agents_to_duty_entry(entry: &mut DutyRoleEntry, spec: DutyStartSpec) -> DutyFeedEvent {
    let clamped = clamp_duty_agent_count(spec.agent_count);
    let additional = build_duty_agents(
        entry.role,
        &entry.agents,
        clamped,
        spec.provider,
        &spec.model_name,
        spec.has_open_pull_requests,
    );
    entry.agents.extend(additional);
    entry.pr_rules = spec.pr_rules;
    entry.scout_rules = spec.scout_rules;
    let suffix = if clamped == 1 { "" } else { "s" };
    let event = DutyFeedEvent {
        timestamp: spec.timestamp,
        actor: "you".into(),
        kind: DutyEventKind::You,
        message: format!("Added {clamped} {} agent{suffix}", spec.model_name),
    };
    entry.feed.push(event.clone());
    event
}

pub fn toggle_duty_pause(entry: &mut DutyRoleEntry, timestamp: String) -> DutyFeedEvent {
    entry.paused = !entry.paused;
    let message = if entry.paused && entry.role == DutyRole::Scouts {
        "Paused duty · in-flight sweeps finish, no new sweep starts"
    } else if entry.paused && entry.role == DutyRole::MainWatcher {
        "Paused duty · a running watch finishes, no new watch starts"
    } else if entry.paused {
        "Paused duty · in-flight reviews finish, nothing new gets claimed"
    } else {
        "Resumed duty"
    };
    let event = DutyFeedEvent {
        timestamp,
        actor: "you".into(),
        kind: DutyEventKind::You,
        message: message.into(),
    };
    entry.feed.push(event.clone());
    event
}

/// Takes `nickname` off the role (F7 Dismiss) and posts who was dismissed, and the PR it let
/// go when it held one. `None` when no agent on the role has that nick.
pub fn dismiss_duty_agent(
    entry: &mut DutyRoleEntry,
    nickname: &str,
    released: Option<u32>,
    timestamp: String,
) -> Option<DutyFeedEvent> {
    let index = entry
        .agents
        .iter()
        .position(|agent| agent.nickname == nickname)?;
    entry.agents.remove(index);
    let message = match released {
        Some(number) => format!("Dismissed {nickname} · released #{number}"),
        None => format!("Dismissed {nickname}"),
    };
    let event = DutyFeedEvent::new(&timestamp, "you", DutyEventKind::You, message);
    entry.feed.push(event.clone());
    Some(event)
}

/// The feed line for a stopped role (F7 Stop). The role and its feed are gone; this line
/// goes out over `duty://event` only.
pub fn stop_duty_event(role: DutyRole, agent_count: usize, timestamp: &str) -> DutyFeedEvent {
    let suffix = if agent_count == 1 { "" } else { "s" };
    let message = format!(
        "Stopped {} · dismissed {agent_count} agent{suffix}",
        role.title().to_lowercase()
    );
    DutyFeedEvent::new(timestamp, "you", DutyEventKind::You, message)
}

pub fn record_duty_instruction(
    entry: &mut DutyRoleEntry,
    prompt: String,
    timestamp: String,
) -> DutyFeedEvent {
    let event = DutyFeedEvent {
        timestamp,
        actor: "you".into(),
        kind: DutyEventKind::You,
        message: prompt,
    };
    entry.feed.push(event.clone());
    event
}
