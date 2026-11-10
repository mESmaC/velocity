//! Read models for F15 drawn in Rust: Settings → Providers, the composer's model menu and the
//! footer limit meter. The window only renders them and sends choices back.

use serde::{Deserialize, Serialize};
use specta::Type;
use velocity_core::layout_view::ProviderUsage;
use velocity_core::thread_view::project_model_pill;
use velocity_core::thread_view_model::ModelPillView;

use crate::project_layout::provider_color;
use crate::providers::{PROVIDERS, ProviderLimit, provider_facts};
use crate::threads::ThreadError;
use crate::{EffortLevel, ProviderKind, Settings, SpeedMode, StateStore, ThreadItem, ThreadState};

/// Where a provider's sign-in stands right now. Only the running app knows this; the saved
/// catalog holds the last answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum ProviderPhase {
    /// Its agent is being asked.
    Checking,
    /// Its agent's command isn't on `PATH`.
    NotInstalled,
    SignedOut,
    /// Waiting for you to finish signing in in the browser.
    Waiting,
    SignedIn,
    /// The agent started but couldn't be read.
    Failed,
}

/// One provider's live state, from the app's provider runtime.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProviderRuntime {
    pub provider: ProviderKind,
    pub phase: ProviderPhase,
    /// The ACP agent command, e.g. `claude-agent-acp · stdio`.
    pub command: String,
    /// Whether the agent offers a sign-in method Velocity may use (never an API key).
    pub can_sign_in: bool,
    /// Why the last check or sign-in failed.
    pub detail: Option<String>,
}

impl ProviderRuntime {
    /// A provider whose agent hasn't answered yet.
    pub fn new(provider: ProviderKind, command: String) -> Self {
        Self {
            provider,
            phase: ProviderPhase::Checking,
            command,
            can_sign_in: false,
            detail: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct LimitView {
    pub percent_used: u8,
    /// `38% of 5-hour · resets in 1h 52m`.
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ModelChipView {
    pub model: String,
    pub shown: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProviderCardView {
    pub provider: ProviderKind,
    pub name: String,
    /// `runs Claude Code over ACP · OAuth`.
    pub runs: String,
    pub command: String,
    pub phase: ProviderPhase,
    pub status: String,
    pub sign_in_label: String,
    pub can_sign_in: bool,
    pub limit: Option<LimitView>,
    pub models: Vec<ModelChipView>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ModelCardView {
    pub provider: ProviderKind,
    pub model: String,
    /// The provider's name.
    pub note: String,
    pub selected: bool,
}

/// A dashed card for a provider you can't pick yet; clicking it starts its sign-in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SignInCardView {
    pub provider: ProviderKind,
    pub name: String,
    pub note: String,
    pub can_sign_in: bool,
}

/// Model cards plus effort and speed: the default for new threads, or the title model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ModelChoiceView {
    pub cards: Vec<ModelCardView>,
    pub sign_in: Vec<SignInCardView>,
    pub effort: EffortLevel,
    pub speed: SpeedMode,
    /// The chosen model offers a fast mode; otherwise the Speed control is greyed out.
    pub speed_available: bool,
    /// `<model> has no fast mode`.
    pub speed_note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProvidersView {
    pub cards: Vec<ProviderCardView>,
    /// `2/4` signed in, for the settings navigation.
    pub signed_in_label: String,
    pub defaults: ModelChoiceView,
    pub titles: ModelChoiceView,
    /// `Titles use GPT-6 Luna · Low · Fast · the default`, or why they use another model.
    pub title_note: String,
    pub title_is_default: bool,
    /// The title model's provider isn't signed in.
    pub title_warning: bool,
}

/// What the views read besides saved settings.
pub struct ProviderContext<'a> {
    pub runtime: &'a [ProviderRuntime],
    /// Now, in Unix seconds, for "resets in …".
    pub now: u32,
}

impl<'a> ProviderContext<'a> {
    pub fn new(runtime: &'a [ProviderRuntime], now: u32) -> Self {
        Self { runtime, now }
    }
}

impl ProviderContext<'_> {
    fn phase(&self, settings: &Settings, provider: ProviderKind) -> ProviderPhase {
        let live = self
            .runtime
            .iter()
            .find(|entry| entry.provider == provider)
            .map_or(ProviderPhase::Checking, |entry| entry.phase);
        // Your sign-out wins over what the agent's login says (F15 Sign out).
        if live == ProviderPhase::SignedIn && !settings.provider_usable(provider) {
            ProviderPhase::SignedOut
        } else {
            live
        }
    }

    fn runtime(&self, provider: ProviderKind) -> Option<&ProviderRuntime> {
        self.runtime.iter().find(|entry| entry.provider == provider)
    }

    /// Whether Sign in can do anything: the agent offers a usable method, or its login still
    /// holds and only Velocity's sign-out stands in the way.
    fn can_sign_in(&self, provider: ProviderKind) -> bool {
        self.runtime(provider)
            .is_some_and(|entry| entry.can_sign_in || entry.phase == ProviderPhase::SignedIn)
    }

    /// The note a signed-out provider shows in pickers.
    fn signed_out_note(&self, settings: &Settings, provider: ProviderKind) -> &'static str {
        match self.phase(settings, provider) {
            ProviderPhase::Waiting => "finishing sign-in…",
            ProviderPhase::Checking => "checking…",
            ProviderPhase::NotInstalled => "not installed",
            _ if !self.can_sign_in(provider) => "no sign-in available",
            _ => "not signed in · Sign in",
        }
    }
}

impl StateStore {
    pub fn providers_view(&self, context: &ProviderContext<'_>) -> ProvidersView {
        let settings = &self.state.settings;
        let cards: Vec<ProviderCardView> = PROVIDERS
            .into_iter()
            .map(|provider| self.provider_card(provider, context))
            .collect();
        let signed_in = cards
            .iter()
            .filter(|card| card.phase == ProviderPhase::SignedIn)
            .count();
        let providers = &settings.providers;
        let defaults = choice_view(
            settings,
            context,
            (
                providers.default_provider,
                &settings.provider_model(providers.default_provider),
            ),
            (providers.default_effort, providers.default_speed),
        );
        let titles = choice_view(
            settings,
            context,
            (providers.title_provider, &providers.title_model),
            (providers.title_effort, providers.title_speed),
        );
        let (title_note, title_is_default, title_warning) = title_note(settings);
        ProvidersView {
            cards,
            signed_in_label: format!("{signed_in}/{}", PROVIDERS.len()),
            defaults,
            titles,
            title_note,
            title_is_default,
            title_warning,
        }
    }

    fn provider_card(
        &self,
        provider: ProviderKind,
        context: &ProviderContext<'_>,
    ) -> ProviderCardView {
        let settings = &self.state.settings;
        let facts = provider_facts(provider);
        let phase = context.phase(settings, provider);
        let runtime = context.runtime(provider);
        let catalog = settings.provider_catalog(provider);
        let models = catalog
            .map(|entry| {
                entry
                    .models
                    .iter()
                    .map(|model| ModelChipView {
                        model: model.name.clone(),
                        shown: !settings.model_hidden(provider, &model.name),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let plan = catalog.and_then(|entry| entry.plan.clone());
        let status = status_text(
            phase,
            plan.as_deref(),
            self.running_agents(provider),
            runtime,
        );
        ProviderCardView {
            provider,
            name: facts.name.into(),
            runs: format!("runs {} over ACP · OAuth", facts.agent),
            command: runtime
                .map(|entry| entry.command.clone())
                .unwrap_or_default(),
            phase,
            status,
            sign_in_label: facts.sign_in_label.into(),
            can_sign_in: context.can_sign_in(provider),
            limit: (phase == ProviderPhase::SignedIn)
                .then(|| limit_view(catalog.and_then(|entry| entry.limit.as_ref()), context.now)),
            models,
        }
    }

    /// Agents working on `provider` across every project.
    fn running_agents(&self, provider: ProviderKind) -> usize {
        self.state
            .projects
            .iter()
            .flat_map(|project| project.threads.iter())
            .filter(|thread| thread.provider == provider && thread.state == ThreadState::Working)
            .count()
    }
}

/// The footer meter: one bar per signed-in provider (F1, F15).
pub fn provider_usage(settings: &Settings) -> Vec<ProviderUsage> {
    settings
        .usable_providers()
        .into_iter()
        .filter_map(|provider| {
            let catalog = settings.provider_catalog(provider)?;
            let name = provider_facts(provider).name;
            let (percent_used, label) = match &catalog.limit {
                Some(limit) => (
                    limit.percent_used,
                    format!("{name} {}% of {}", limit.percent_used, limit.window),
                ),
                None => (0, format!("{name} · limit not reported")),
            };
            Some(ProviderUsage::new(
                provider_color(provider),
                label,
                percent_used,
            ))
        })
        .collect()
}

/// The home composer's model pill: what a new thread starts on, the default for new threads
/// unless you signed out of it (F4, F15).
pub fn default_model_pill(settings: &Settings) -> ModelPillView {
    let providers = &settings.providers;
    let provider = settings.new_work_choice();
    let model = settings.provider_model(provider);
    let fast = settings.model_has_fast_mode(provider, &model);
    ModelPillView::new(
        provider_color(provider),
        model,
        effort_label(providers.default_effort).into(),
        project_model_pill(providers.default_speed == SpeedMode::Fast, fast),
    )
}

fn status_text(
    phase: ProviderPhase,
    plan: Option<&str>,
    agents: usize,
    runtime: Option<&ProviderRuntime>,
) -> String {
    match phase {
        ProviderPhase::Checking => "Checking…".into(),
        ProviderPhase::NotInstalled => runtime.map_or_else(
            || "Its agent isn't installed".into(),
            |entry| format!("{} isn't installed", command_program(&entry.command)),
        ),
        ProviderPhase::SignedOut => {
            "Not signed in · its models are hidden from every picker".into()
        }
        ProviderPhase::Waiting => "Waiting for you to finish signing in in your browser…".into(),
        ProviderPhase::Failed => runtime
            .and_then(|entry| entry.detail.clone())
            .unwrap_or_else(|| "Could not read its agent".into()),
        ProviderPhase::SignedIn => {
            let plan = plan.map(|plan| format!(" · {plan}")).unwrap_or_default();
            let agents = match agents {
                0 => String::new(),
                1 => " · 1 agent".into(),
                count => format!(" · {count} agents"),
            };
            format!("Signed in{plan}{agents}")
        }
    }
}

fn command_program(command: &str) -> &str {
    command.split(" · ").next().unwrap_or(command)
}

/// `38% of 5-hour · resets in 1h 52m`, or "limit not reported" (F15 Limit sources).
pub fn limit_view(limit: Option<&ProviderLimit>, now: u32) -> LimitView {
    let Some(limit) = limit else {
        return LimitView {
            percent_used: 0,
            label: "limit not reported".into(),
        };
    };
    let resets = limit
        .resets_at
        .map(|at| format!(" · resets in {}", duration_label(at.saturating_sub(now))))
        .unwrap_or_default();
    LimitView {
        percent_used: limit.percent_used,
        label: format!("{}% of {}{resets}", limit.percent_used, limit.window),
    }
}

/// `1h 52m` until `limit` resets, or `soon` when it gave no time.
pub fn resets_in(limit: &ProviderLimit, now: u32) -> String {
    limit.resets_at.map_or_else(
        || "soon".into(),
        |at| duration_label(at.saturating_sub(now)),
    )
}

fn duration_label(seconds: u32) -> String {
    let minutes = seconds / 60;
    let (days, hours, minutes) = (minutes / 1_440, minutes / 60 % 24, minutes % 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, hours) => format!("{hours}h {minutes}m"),
        (days, hours) => format!("{days}d {hours}h"),
    }
}

fn choice_view(
    settings: &Settings,
    context: &ProviderContext<'_>,
    (provider, model): (ProviderKind, &str),
    (effort, speed): (EffortLevel, SpeedMode),
) -> ModelChoiceView {
    let cards = settings
        .usable_providers()
        .into_iter()
        .flat_map(|candidate| {
            let note = provider_facts(candidate).name;
            settings
                .visible_models(candidate)
                .into_iter()
                .map(move |entry| ModelCardView {
                    provider: candidate,
                    model: entry.name.clone(),
                    note: note.into(),
                    selected: candidate == provider && entry.name == model,
                })
        })
        .collect();
    let fast = settings.model_has_fast_mode(provider, model);
    ModelChoiceView {
        cards,
        sign_in: sign_in_cards(settings, context),
        effort,
        speed,
        speed_available: fast,
        speed_note: (!fast).then(|| format!("{model} has no fast mode")),
    }
}

fn sign_in_cards(settings: &Settings, context: &ProviderContext<'_>) -> Vec<SignInCardView> {
    PROVIDERS
        .into_iter()
        .filter(|provider| !settings.provider_usable(*provider))
        .map(|provider| SignInCardView {
            provider,
            name: provider_facts(provider).name.into(),
            note: context.signed_out_note(settings, provider).into(),
            can_sign_in: context.can_sign_in(provider),
        })
        .collect()
}

fn title_note(settings: &Settings) -> (String, bool, bool) {
    let providers = &settings.providers;
    let defaults = Settings::default().providers;
    let is_default = providers.title_provider == defaults.title_provider
        && providers.title_model == defaults.title_model
        && providers.title_effort == defaults.title_effort
        && providers.title_speed == defaults.title_speed;
    if !settings.provider_usable(providers.title_provider) {
        let fallback = settings
            .usable_providers()
            .first()
            .map(|provider| settings.provider_model(*provider))
            .unwrap_or_else(|| "no model".into());
        let name = provider_facts(providers.title_provider).name;
        let note = format!("{name} isn't signed in · titles use {fallback} · Low until it is");
        return (note, is_default, true);
    }
    let fast = settings.model_has_fast_mode(providers.title_provider, &providers.title_model)
        && providers.title_speed == SpeedMode::Fast;
    let note = format!(
        "Titles use {} · {}{}",
        providers.title_model,
        effort_label(providers.title_effort),
        if fast { " · Fast" } else { "" }
    );
    (note, is_default, false)
}

pub fn effort_label(effort: EffortLevel) -> &'static str {
    match effort {
        EffortLevel::Low => "Low",
        EffortLevel::Medium => "Medium",
        EffortLevel::High => "High",
        EffortLevel::Max => "Max",
    }
}

/// The composer's model menu (F4): grouped by provider, effort and speed at the bottom.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ModelMenuView {
    /// The thread the menu changes, or `None` for the home composer (the defaults).
    pub thread_identifier: Option<u32>,
    pub groups: Vec<ModelMenuGroup>,
    pub effort: EffortLevel,
    pub speed: SpeedMode,
    pub speed_available: bool,
    pub speed_note: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ModelMenuGroup {
    pub provider: ProviderKind,
    pub name: String,
    /// `38% of weekly`, `signed out`, `signing in…`.
    pub note: String,
    pub signed_in: bool,
    pub sign_in_label: String,
    pub can_sign_in: bool,
    pub models: Vec<ModelMenuItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ModelMenuItem {
    pub model: String,
    pub selected: bool,
    /// A running thread stays on its provider's agent; other providers' models are listed
    /// but can't be picked for it.
    pub enabled: bool,
}

/// A pick from the model menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum ModelMenuChoice {
    Model {
        provider: ProviderKind,
        model: String,
    },
    Effort(EffortLevel),
    Speed(SpeedMode),
}

/// What the menu is for: a thread's current choice, or the defaults.
pub struct MenuTarget<'a> {
    pub thread: Option<&'a ThreadItem>,
    /// The provider a live session is bound to, when the thread has one.
    pub live_provider: Option<ProviderKind>,
}

impl<'a> MenuTarget<'a> {
    pub fn new(thread: Option<&'a ThreadItem>, live_provider: Option<ProviderKind>) -> Self {
        Self {
            thread,
            live_provider,
        }
    }
}

impl StateStore {
    pub fn model_menu(
        &self,
        target: &MenuTarget<'_>,
        context: &ProviderContext<'_>,
    ) -> ModelMenuView {
        let settings = &self.state.settings;
        let providers = &settings.providers;
        let (provider, model, effort, speed) = match target.thread {
            Some(thread) => (
                thread.provider,
                thread.model_name.clone(),
                thread.effort,
                thread.preferred_speed,
            ),
            None => (
                settings.new_work_choice(),
                settings.provider_model(settings.new_work_choice()),
                providers.default_effort,
                providers.default_speed,
            ),
        };
        let groups = PROVIDERS
            .into_iter()
            .map(|candidate| {
                menu_group(
                    settings,
                    context,
                    candidate,
                    (provider, &model),
                    target.live_provider,
                )
            })
            .collect();
        let fast = settings.model_has_fast_mode(provider, &model);
        ModelMenuView {
            thread_identifier: target.thread.map(|thread| thread.id),
            groups,
            effort,
            speed,
            speed_available: fast,
            speed_note: (!fast).then(|| format!("no fast mode on {model}")),
        }
    }

    /// Applies a menu pick to a thread (F4): its next message runs on it.
    pub fn choose_thread_model(
        &mut self,
        project: u32,
        thread: u32,
        choice: ModelMenuChoice,
    ) -> Result<bool, ThreadError> {
        let settings = self.state.settings.clone();
        self.update_thread(project, thread, |item| match choice {
            ModelMenuChoice::Model { provider, model } => {
                let fast = settings.model_has_fast_mode(provider, &model);
                item.provider = provider;
                *item = item.clone().switch_model(model, fast);
            }
            ModelMenuChoice::Effort(effort) => item.effort = effort,
            ModelMenuChoice::Speed(speed) => {
                item.preferred_speed = speed;
                item.effective_speed = if item.model_supports_fast {
                    speed
                } else {
                    SpeedMode::Regular
                };
            }
        })
    }
}

fn menu_group(
    settings: &Settings,
    context: &ProviderContext<'_>,
    candidate: ProviderKind,
    (provider, model): (ProviderKind, &str),
    live_provider: Option<ProviderKind>,
) -> ModelMenuGroup {
    let facts = provider_facts(candidate);
    let signed_in = settings.provider_usable(candidate);
    let note = if signed_in {
        let limit = settings
            .provider_catalog(candidate)
            .and_then(|entry| entry.limit.as_ref());
        match limit {
            Some(limit) => format!("{}% of {}", limit.percent_used, limit.window),
            None => "limit not reported".into(),
        }
    } else {
        match context.phase(settings, candidate) {
            ProviderPhase::Waiting => "signing in…".into(),
            ProviderPhase::NotInstalled => "not installed".into(),
            _ => "signed out".into(),
        }
    };
    let enabled = live_provider.is_none_or(|live| live == candidate);
    let models = if signed_in {
        settings
            .visible_models(candidate)
            .into_iter()
            .map(|entry| ModelMenuItem {
                model: entry.name.clone(),
                selected: candidate == provider && entry.name == model,
                enabled,
            })
            .collect()
    } else {
        Vec::new()
    };
    ModelMenuGroup {
        provider: candidate,
        name: facts.name.into(),
        note,
        signed_in,
        sign_in_label: facts.sign_in_label.into(),
        can_sign_in: context.can_sign_in(candidate),
        models,
    }
}
