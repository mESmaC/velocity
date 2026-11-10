//! The Goal swarm dialog's read model (SPEC.md F16): the agent count, the model mix with
//! "the rest", the nick preview, limits, the pool line and the PR duty capacity check. The
//! window holds only the user's choices: it sends them back with one edit, and Rust applies
//! the edit and draws the dialog again.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;
use velocity_core::goal_swarm::{self, Mix, MixEdit, MixRow, PoolCounts, PoolLine};
use velocity_core::state::resolve_effective_speed;

use crate::swarm_dialog::{pr_duty, provider_model_name};
use crate::{EffortLevel, ProjectItem, ProviderKind, Settings, SpeedMode, StateStore};

/// One mix row as the window holds it: a provider's model, its effort and speed, and how
/// many agents run on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct GoalMixRow {
    pub provider: ProviderKind,
    pub effort: EffortLevel,
    pub speed: SpeedMode,
    pub count: u32,
}

impl GoalMixRow {
    pub fn new(provider: ProviderKind, effort: EffortLevel, speed: SpeedMode, count: u32) -> Self {
        Self {
            provider,
            effort,
            speed,
            count,
        }
    }
}

/// The user's choices. The first row is "the rest".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct GoalSwarmChoices {
    pub agents: u32,
    pub rows: Vec<GoalMixRow>,
}

impl GoalSwarmChoices {
    pub fn new(agents: u32, rows: Vec<GoalMixRow>) -> Self {
        Self { agents, rows }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoalMixEdit {
    SetAgents { count: u32 },
    StepAgents { up: bool },
    StepRow { index: u32, up: bool },
    SetModel { index: u32, provider: ProviderKind },
    SetEffort { index: u32, effort: EffortLevel },
    ToggleFast { index: u32 },
    RemoveRow { index: u32 },
    AddRow,
    EvenSplit,
}

/// `choices: None` asks for the defaults: every agent on the default model.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct GoalSwarmDialogQuery {
    pub choices: Option<GoalSwarmChoices>,
    pub edit: Option<GoalMixEdit>,
}

impl GoalSwarmDialogQuery {
    pub fn with_choices(choices: GoalSwarmChoices) -> Self {
        Self {
            choices: Some(choices),
            edit: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct GoalModelOption {
    pub provider: ProviderKind,
    pub provider_name: String,
    pub model_name: String,
}

/// One block of the split bar above the rows, as wide as its share of the agents.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct GoalMixSegment {
    pub provider: ProviderKind,
    pub weight: u32,
    pub label: String,
    pub title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct GoalMixRowView {
    pub provider: ProviderKind,
    pub effort: EffortLevel,
    /// Fast is on and the model offers it.
    pub fast: bool,
    pub fast_available: bool,
    pub fast_title: String,
    pub count: u32,
    pub rest: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct GoalLimitLine {
    pub provider: ProviderKind,
    pub text: String,
    pub warn: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct GoalSwarmDialog {
    pub project_identifier: u32,
    /// The project's AIRC channel, `#<project>`.
    pub channel: String,
    pub worktree_root: String,
    pub default_branch: String,
    pub choices: GoalSwarmChoices,
    pub quick_picks: Vec<u32>,
    pub worktrees_hint: String,
    pub models: Vec<GoalModelOption>,
    pub segments: Vec<GoalMixSegment>,
    pub rows: Vec<GoalMixRowView>,
    pub nick_preview: String,
    pub limits: Vec<GoalLimitLine>,
    pub pool: Option<PoolLine>,
    /// Shown in place of the pool line while it is being read, or when reading it failed.
    pub pool_note: Option<String>,
    pub pr_duty_label: Option<String>,
    pub pr_duty_shortfall: u32,
    pub summary: String,
    pub launch_label: String,
    /// Why Launch is unavailable, when it is.
    pub launch_note: Option<String>,
}

/// The project's work pool as the dialog last read it (F16 Pool).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoalPool {
    Reading,
    Read(PoolCounts),
    Failed(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MixModel {
    provider: ProviderKind,
    effort: EffortLevel,
    speed: SpeedMode,
}

impl StateStore {
    pub fn goal_swarm_dialog(
        &self,
        query: &GoalSwarmDialogQuery,
        pool: &GoalPool,
    ) -> Option<GoalSwarmDialog> {
        let project = self.active_project()?;
        let settings = &self.state.settings;
        let mix = edited_mix(settings, query);
        let agents = mix.agents;
        let preferences = self.worktree_context().preferences;
        let root = preferences.project_override.unwrap_or(preferences.root);
        let (pool_line, pool_note, pull_requests) = match pool {
            GoalPool::Reading => (None, Some("Reading the work pool\u{2026}".into()), 0),
            GoalPool::Failed(error) => (None, Some(error.clone()), 0),
            GoalPool::Read(counts) => (
                Some(goal_swarm::pool_line(*counts, agents)),
                None,
                counts.new_pull_requests(agents),
            ),
        };
        let (pr_duty_label, pr_duty_shortfall) = pr_duty(
            project,
            usize::try_from(pull_requests).unwrap_or(usize::MAX),
        );
        Some(GoalSwarmDialog {
            project_identifier: project.id,
            channel: format!("#{}", project.name),
            default_branch: project.default_branch.clone(),
            quick_picks: goal_swarm::QUICK_PICKS.to_vec(),
            worktrees_hint: goal_swarm::worktrees_hint(agents, &root),
            worktree_root: root,
            models: model_options(settings),
            segments: segments(settings, &mix),
            rows: mix.rows.iter().zip(0..).map(row_view(settings)).collect(),
            nick_preview: nick_preview(settings, project, &mix),
            limits: limits(settings, &mix),
            pool: pool_line,
            pool_note,
            pr_duty_label,
            pr_duty_shortfall,
            summary: summary(settings, &mix),
            launch_label: goal_swarm::launch_label(agents),
            launch_note: crate::build_gate::require_launch(
                &self.state,
                crate::build_gate::LaunchRequest::GoalSwarm,
            )
            .err()
            .map(|error| error.to_string()),
            choices: choices(&mix),
        })
    }
}

/// The user's mix with their edit applied, always holding at least "the rest".
fn edited_mix(settings: &Settings, query: &GoalSwarmDialogQuery) -> Mix<MixModel> {
    let providers = &settings.providers;
    let usable = settings.usable_providers();
    let fallback = MixModel {
        provider: launchable(&usable, providers.default_provider)
            .or_else(|| usable.first().copied())
            .unwrap_or(providers.default_provider),
        effort: providers.default_effort,
        speed: providers.default_speed,
    };
    let mut mix = Mix {
        agents: goal_swarm::DEFAULT_AGENTS,
        rows: Vec::new(),
    };
    if let Some(choices) = &query.choices {
        mix.agents = choices.agents;
        mix.rows = choices
            .rows
            .iter()
            .map(|row| MixRow {
                model: MixModel {
                    provider: launchable(&usable, row.provider).unwrap_or(fallback.provider),
                    effort: row.effort,
                    speed: row.speed,
                },
                count: row.count,
            })
            .collect();
    }
    if mix.rows.is_empty() {
        mix.rows.push(MixRow {
            model: fallback,
            count: mix.agents,
        });
    }
    match query.edit {
        Some(edit) => apply(&mut mix, edit, fallback, &usable),
        None => mix.settle(),
    }
    mix
}

fn apply(mix: &mut Mix<MixModel>, edit: GoalMixEdit, fallback: MixModel, usable: &[ProviderKind]) {
    let position = |index: u32| usize::try_from(index).unwrap_or(usize::MAX);
    let mut change = |index: u32, set: &dyn Fn(&mut MixModel)| {
        if let Some(row) = mix.rows.get_mut(position(index)) {
            set(&mut row.model);
        }
    };
    let counted = match edit {
        GoalMixEdit::SetAgents { count } => Some(MixEdit::SetAgents(count)),
        GoalMixEdit::StepAgents { up } => Some(MixEdit::StepAgents { up }),
        GoalMixEdit::StepRow { index, up } => Some(MixEdit::StepRow {
            index: position(index),
            up,
        }),
        GoalMixEdit::RemoveRow { index } => Some(MixEdit::RemoveRow(position(index))),
        GoalMixEdit::EvenSplit => Some(MixEdit::EvenSplit),
        GoalMixEdit::AddRow => Some(MixEdit::AddRow(MixModel {
            provider: unused_provider(mix, usable).unwrap_or(fallback.provider),
            ..fallback
        })),
        GoalMixEdit::SetModel { index, provider } => {
            if let Some(provider) = launchable(usable, provider) {
                change(index, &|model| model.provider = provider);
            }
            None
        }
        GoalMixEdit::SetEffort { index, effort } => {
            change(index, &|model| model.effort = effort);
            None
        }
        GoalMixEdit::ToggleFast { index } => {
            change(index, &|model| {
                model.speed = match model.speed {
                    SpeedMode::Fast => SpeedMode::Regular,
                    SpeedMode::Regular => SpeedMode::Fast,
                };
            });
            None
        }
    };
    match counted {
        Some(edit) => mix.apply(edit),
        None => mix.settle(),
    }
}

/// Only signed-in providers' models can be picked (F15).
fn launchable(usable: &[ProviderKind], provider: ProviderKind) -> Option<ProviderKind> {
    usable.contains(&provider).then_some(provider)
}

/// Add a model starts on the first model no row runs yet.
fn unused_provider(mix: &Mix<MixModel>, usable: &[ProviderKind]) -> Option<ProviderKind> {
    usable
        .iter()
        .copied()
        .find(|provider| mix.rows.iter().all(|row| row.model.provider != *provider))
}

fn effort_name(effort: EffortLevel) -> &'static str {
    match effort {
        EffortLevel::Low => "Low",
        EffortLevel::Medium => "Medium",
        EffortLevel::High => "High",
        EffortLevel::Max => "Max",
    }
}

/// Whether Fast is offered for a provider's model. An agent reports its fast mode in its
/// ACP config options once a session runs (§3.2), and nothing reads them ahead of a launch
/// until Settings → Providers lists models (F15). Until then Fast stays selectable, and a
/// session on a model without it runs at Regular.
fn offers_fast(_provider: ProviderKind) -> bool {
    true
}

fn runs_fast(model: MixModel) -> bool {
    let effective = resolve_effective_speed(offers_fast(model.provider), model.speed.into());
    SpeedMode::from(effective) == SpeedMode::Fast
}

fn model_options(settings: &Settings) -> Vec<GoalModelOption> {
    settings
        .usable_providers()
        .into_iter()
        .map(|provider| GoalModelOption {
            provider,
            provider_name: settings.provider_name(provider),
            model_name: provider_model_name(settings, provider),
        })
        .collect()
}

fn row_view(settings: &Settings) -> impl Fn((&MixRow<MixModel>, usize)) -> GoalMixRowView + '_ {
    move |(row, index)| {
        let available = offers_fast(row.model.provider);
        GoalMixRowView {
            provider: row.model.provider,
            effort: row.model.effort,
            fast: runs_fast(row.model),
            fast_available: available,
            fast_title: if available {
                "Fast mode".into()
            } else {
                format!(
                    "{} has no fast mode",
                    provider_model_name(settings, row.model.provider)
                )
            },
            count: row.count,
            rest: index == 0,
        }
    }
}

fn segments(settings: &Settings, mix: &Mix<MixModel>) -> Vec<GoalMixSegment> {
    mix.rows
        .iter()
        .filter(|row| row.count > 0)
        .map(|row| {
            let name = provider_model_name(settings, row.model.provider);
            GoalMixSegment {
                provider: row.model.provider,
                weight: row.count,
                label: format!("{} {}", row.count, goal_swarm::model_nick(&name)),
                title: format!(
                    "{} on {name} · {}{}",
                    row.count,
                    effort_name(row.model.effort),
                    if runs_fast(row.model) { " · Fast" } else { "" }
                ),
            }
        })
        .collect()
}

fn nick_preview(settings: &Settings, project: &ProjectItem, mix: &Mix<MixModel>) -> String {
    let rows: Vec<(String, u32)> = mix
        .rows
        .iter()
        .map(|row| {
            let name = provider_model_name(settings, row.model.provider);
            (goal_swarm::model_nick(&name), row.count)
        })
        .collect();
    goal_swarm::nick_preview(&rows, &used_nick_numbers(project))
}

/// The highest number each nick stem has used in the project (F14: nicks are counted per
/// project). A goal swarm agent's worktree keeps its nick, and its branch does until it
/// claims a piece.
pub(crate) fn used_nick_numbers(project: &ProjectItem) -> BTreeMap<String, u32> {
    let mut used = BTreeMap::new();
    let record = |used: &mut BTreeMap<String, u32>, name: &str| {
        if let Some((stem, number)) = name.rsplit_once('-')
            && let Ok(number) = number.parse::<u32>()
        {
            let highest = used.entry(stem.to_owned()).or_insert(0);
            *highest = number.max(*highest);
        }
    };
    for thread in &project.threads {
        let leaf = thread
            .worktree_path
            .as_deref()
            .and_then(|path| path.rsplit(['\\', '/']).next());
        let branch = thread.branch_name.strip_prefix("vel/");
        for name in [leaf, branch].into_iter().flatten() {
            record(&mut used, name);
        }
    }
    for member in project.goal_swarms.iter().flat_map(|swarm| &swarm.members) {
        record(&mut used, &member.nick);
    }
    used
}

/// One line per provider in the mix, with the share of its window used where the provider
/// reports one (F15 Limit sources).
fn limits(settings: &Settings, mix: &Mix<MixModel>) -> Vec<GoalLimitLine> {
    let now = crate::providers::unix_now();
    settings
        .all_providers()
        .into_iter()
        .filter_map(|provider| {
            let agents: u32 = mix
                .rows
                .iter()
                .filter(|row| row.model.provider == provider)
                .map(|row| row.count)
                .sum();
            let usage = settings
                .provider_catalog(provider)
                .and_then(|entry| entry.limit.as_ref())
                .map(|limit| goal_swarm::LimitUsage {
                    percent_used: limit.percent_used,
                    window: limit.window.clone(),
                    resets_in: crate::provider_views::resets_in(limit, now),
                });
            let (text, warn) =
                goal_swarm::limit_line(&settings.provider_name(provider), agents, usage.as_ref());
            (agents > 0).then_some(GoalLimitLine {
                provider,
                text,
                warn,
            })
        })
        .collect()
}

fn summary(settings: &Settings, mix: &Mix<MixModel>) -> String {
    let rows: Vec<GoalMixRow> = mix
        .rows
        .iter()
        .map(|row| GoalMixRow {
            provider: row.model.provider,
            effort: row.model.effort,
            speed: row.model.speed,
            count: row.count,
        })
        .collect();
    mix_rows_summary(settings, &rows)
}

pub(crate) fn mix_rows_summary(settings: &Settings, rows: &[GoalMixRow]) -> String {
    rows.iter()
        .filter(|row| row.count > 0)
        .map(|row| {
            format!(
                "{} {} {}",
                row.count,
                provider_model_name(settings, row.provider),
                effort_name(row.effort)
            )
        })
        .collect::<Vec<_>>()
        .join(" \u{00b7} ")
}

fn choices(mix: &Mix<MixModel>) -> GoalSwarmChoices {
    GoalSwarmChoices {
        agents: mix.agents,
        rows: mix
            .rows
            .iter()
            .map(|row| GoalMixRow {
                provider: row.model.provider,
                effort: row.model.effort,
                speed: row.model.speed,
                count: row.count,
            })
            .collect(),
    }
}
