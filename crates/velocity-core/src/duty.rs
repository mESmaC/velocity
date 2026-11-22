//! Duty role domain types, rules, agents, activity feed, and state transitions (`SPEC.md §4 F7`, `§7 M5`).

use crate::layout_view::ProviderColor;
use crate::thread_view_model::ModelPillView;

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DutyRole {
    #[default]
    Scouts,
}

impl DutyRole {
    pub fn title(self) -> &'static str {
        "Scouts"
    }

    pub fn prefix(self) -> &'static str {
        "scout"
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
    DevelopmentSpeed,
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
            Self::DevelopmentSpeed => "Development speed",
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
            Self::DevelopmentSpeed => "development speed",
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

#[cfg(feature = "bindings")]
#[derive(serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename = "ScoutCadence", rename_all = "snake_case")]
enum ScoutCadenceBinding {
    Once,
    EveryThirtyMinutes,
    AfterEveryLand,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoutCadence {
    Once,
    #[default]
    EveryThirtyMinutes,
    #[serde(alias = "after_every_merge")]
    AfterEveryLand,
}

impl ScoutCadence {
    pub fn header_label(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::EveryThirtyMinutes => "30m minimum",
            Self::AfterEveryLand => "after every land",
        }
    }

    pub fn rule_label(self) -> &'static str {
        match self {
            Self::Once => "Sweep once",
            Self::EveryThirtyMinutes => "At least 30m between scheduled sweeps",
            Self::AfterEveryLand => "Sweep after every land",
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
    #[cfg_attr(feature = "bindings", specta(type = ScoutCadenceBinding))]
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
    /// Whether this scout revisits an unchanged base for new generated inputs.
    #[cfg_attr(feature = "bindings", serde(skip))]
    #[cfg_attr(feature = "bindings", specta(skip))]
    pub repeat_unchanged: bool,
    /// The effort and speed its sessions run at (F7, F15). `None` on an agent saved before
    /// they could be chosen: its sessions use Settings' defaults. Saved state only.
    #[cfg_attr(feature = "bindings", serde(skip))]
    #[cfg_attr(feature = "bindings", specta(skip))]
    pub effort: Option<crate::state::EffortLevel>,
    #[cfg_attr(feature = "bindings", serde(skip))]
    #[cfg_attr(feature = "bindings", specta(skip))]
    pub speed: Option<crate::state::SpeedMode>,
}

#[cfg(feature = "bindings")]
#[derive(serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(rename = "DutyEventKind", rename_all = "snake_case")]
pub enum DutyEventKindBinding {
    Land,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DutyEventKind {
    #[serde(alias = "merge")]
    Land,
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

impl DutyEventKind {
    #[allow(non_upper_case_globals)]
    pub const Merge: Self = Self::Land;
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DutyFeedEvent {
    pub timestamp: String,
    pub actor: String,
    #[cfg_attr(feature = "bindings", specta(type = DutyEventKindBinding))]
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
    pub scout_rules: ScoutRuleSettings,
    pub agents: Vec<DutyAgentCard>,
    pub feed: Vec<DutyFeedEvent>,
    /// Only after-land Scouts use this mode. The existing duty IPC remains the same;
    /// coverage is rendered as the duty's title, rules and cadence line.
    pub review_swarm: Option<ReviewSwarmCoverage>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReviewSwarmCoverage {
    pub unreviewed: usize,
    pub oldest_seconds: u64,
}

impl ReviewSwarmCoverage {
    pub fn new(unreviewed: usize, oldest_seconds: u64) -> Self {
        Self {
            unreviewed,
            oldest_seconds,
        }
    }
}

impl DutyRoleEntry {
    pub fn new(identifier: usize, role: DutyRole) -> Self {
        Self {
            identifier,
            role,
            paused: false,
            issues_filed: 0,
            scout_rules: ScoutRuleSettings::default(),
            agents: Vec::new(),
            feed: Vec::new(),
            review_swarm: None,
        }
    }

    pub fn title(&self) -> &'static str {
        if self.review_swarm.is_some() {
            "Review swarm"
        } else {
            self.role.title()
        }
    }

    pub fn coverage_label(&self) -> Option<String> {
        let coverage = self.review_swarm.as_ref()?;
        let window = review_window_seconds(self.scout_rules.cadence) / 60;
        Some(format!(
            "{} unreviewed · oldest {}m · window {window}m",
            coverage.unreviewed,
            coverage.oldest_seconds / 60
        ))
    }
}

/// Cadence is the existing duty setting: after every land requests a one-minute
/// response, periodic sweeps a thirty-minute response. Once still drains all reviews.
pub fn review_window_seconds(cadence: ScoutCadence) -> u64 {
    match cadence {
        ScoutCadence::AfterEveryLand => 60,
        ScoutCadence::Once | ScoutCadence::EveryThirtyMinutes => 30 * 60,
    }
}

pub fn clamp_duty_agent_count(count: usize) -> usize {
    count.clamp(1, 4)
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

fn format_focus_areas(areas: &[ScoutFocusArea]) -> Option<String> {
    let first = areas.first()?;
    let mut text = String::from(first.label());
    for area in &areas[1..] {
        text.push_str(" · ");
        text.push_str(area.summary_token());
    }
    Some(text)
}

pub fn duty_sidebar_summary(entry: &DutyRoleEntry) -> String {
    if entry.paused {
        return "paused".into();
    }
    if let Some(coverage) = entry.coverage_label() {
        return coverage;
    }
    let doing = entry
        .agents
        .first()
        .map_or("idle", |agent| agent.doing.as_str());
    format!("{} filed · {doing}", entry.issues_filed)
}

pub fn project_duty_view(entry: &DutyRoleEntry, default_pill: &ModelPillView) -> DutyViewSnapshot {
    let count = entry.agents.len();
    let agent_count_label = if count == 1 {
        "1 agent".into()
    } else {
        format!("{count} agents")
    };
    let model_pill = entry.agents.first().map_or_else(
        || default_pill.clone(),
        |agent| ModelPillView {
            usage_account: None,
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
        title: entry.title().into(),
        paused: entry.paused,
        agent_count: count,
        agent_count_label,
        issues_filed: entry.issues_filed,
        cadence_label: entry
            .coverage_label()
            .unwrap_or_else(|| entry.scout_rules.cadence.header_label().into()),
        rules: duty_rules(entry),
        agents: entry.agents.clone(),
        feed: entry.feed.clone(),
        composer_placeholder: duty_placeholder(entry).into(),
        model_pill,
    }
}

fn duty_rules(entry: &DutyRoleEntry) -> Vec<DutyRuleChip> {
    if entry.review_swarm.is_some() {
        return [
            "Lands never wait for review",
            "Sensitive changes first",
            "Evidence attached · no duplicates",
            "Proof-list features verified in Lean",
            "Agent roster caps concurrency",
        ]
        .into_iter()
        .map(|text| DutyRuleChip {
            text: text.into(),
            hard: true,
        })
        .collect();
    }
    build_scout_rule_chips(&entry.scout_rules)
}

fn duty_placeholder(entry: &DutyRoleEntry) -> &'static str {
    if entry.review_swarm.is_some() {
        "Point the review swarm at a feature, issue or proof gap"
    } else {
        "Point the scouts somewhere, e.g. “dig into the IPC layer”"
    }
}

pub struct DutyStartSpec {
    pub role: DutyRole,
    pub agent_count: usize,
    pub provider: ProviderColor,
    pub model_name: String,
    /// The effort and speed the new agents run at; `None` keeps Settings' defaults.
    pub effort: Option<crate::state::EffortLevel>,
    pub speed: Option<crate::state::SpeedMode>,
    pub scout_rules: ScoutRuleSettings,
    pub repeat_unchanged: bool,
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
) -> Vec<DutyAgentCard> {
    let clamped = clamp_duty_agent_count(count);
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
            doing: "waiting to start".into(),
            sweep: None,
            repeat_unchanged: false,
            effort: None,
            speed: None,
        })
        .collect()
}

/// The new agents run at the effort and speed the dialog chose (F7).
fn with_run_choice(mut agents: Vec<DutyAgentCard>, spec: &DutyStartSpec) -> Vec<DutyAgentCard> {
    for agent in &mut agents {
        agent.effort = spec.effort;
        agent.speed = spec.speed;
        agent.repeat_unchanged = spec.role == DutyRole::Scouts && spec.repeat_unchanged;
    }
    agents
}

pub fn start_duty_entry(identifier: usize, spec: DutyStartSpec) -> (DutyRoleEntry, DutyFeedEvent) {
    let clamped = clamp_duty_agent_count(spec.agent_count);
    let agents = build_duty_agents(spec.role, &[], clamped, spec.provider, &spec.model_name);
    let agents = with_run_choice(agents, &spec);
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
        scout_rules: spec.scout_rules,
        agents,
        feed: vec![event.clone()],
        review_swarm: None,
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
    );
    entry.agents.extend(with_run_choice(additional, &spec));
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
    let message = if entry.paused && entry.review_swarm.is_some() {
        "Paused review · in-flight reviews finish"
    } else if entry.paused {
        "Paused Scouts · in-flight sweeps finish"
    } else {
        "Resumed"
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
