//! Naming a build thread with the title model (SPEC.md F4, F15): one request to the title
//! model's agent, run while the thread's worktree is checked out, settled once.

use std::path::PathBuf;
use std::time::Duration;

use velocity_acp::client::{TitleRequest, request_title};
use velocity_acp::{Effort, Provider, RunSettings, Speed};
use velocity_core::thread_titles::{
    TITLE_DEADLINE_MILLISECONDS, ThreadTitle, TitleSource, title_from_response,
    title_request_prompt,
};
use velocity_store::duty::DutyAgentItem;
use velocity_store::{EffortLevel, ProviderKind, Settings, SpeedMode, ThreadItem};

use crate::launch::agent_launch;

/// How a new thread gets its title.
#[derive(Clone, Debug)]
pub struct TitlePlan {
    /// The message's first words: the title until the model answers, and after it fails.
    pub fallback: String,
    /// `None` when there is nothing to ask: the thread keeps its fallback at once.
    pub request: Option<TitleRequest>,
    /// Shown in the header as "titling · <model>".
    pub model_name: String,
}

/// The request for Settings → Providers → Thread titles (F15), started in the project's
/// repository.
pub fn plan_title(
    settings: &Settings,
    repository: PathBuf,
    prompt: &str,
    fallback: String,
) -> TitlePlan {
    let providers = &settings.providers;
    // A signed-out title provider: the first signed-in model titles at Low until it signs in
    // again (F15 Thread titles).
    let (title_provider, model_name, title_effort) =
        if settings.provider_usable(providers.title_provider) {
            (
                providers.title_provider,
                providers.title_model.clone(),
                providers.title_effort,
            )
        } else {
            let first = settings
                .usable_providers()
                .first()
                .copied()
                .unwrap_or(providers.title_provider);
            (first, settings.provider_model(first), EffortLevel::Low)
        };
    let run_settings = RunSettings {
        provider: provider(title_provider),
        model: model_name.clone(),
        effort: effort(title_effort),
        speed: speed(providers.title_speed),
    };
    // A title agent that can't start leaves the message's first words as the title.
    let request = agent_launch(settings, title_provider)
        .ok()
        .map(|(launch, _)| {
            TitleRequest::new(
                launch,
                repository,
                run_settings,
                title_request_prompt(prompt),
                Duration::from_millis(TITLE_DEADLINE_MILLISECONDS),
            )
        });
    TitlePlan {
        fallback,
        request,
        model_name,
    }
}

/// The model's title when it answers in time with something usable, else `None`: the thread
/// keeps its fallback (F4). Each thread calls this once, from its first message.
pub async fn resolve_title(plan: TitlePlan) -> Option<String> {
    let fallback = plan.fallback.clone();
    let title = ThreadTitle::new(TitleSource::Message(plan.fallback), plan.request.is_some());
    let request = plan.request?;
    let settled = match request_title(request).await {
        Ok(reply) => title.settle(
            title_from_response(&reply.text),
            u64::try_from(reply.elapsed.as_millis()).unwrap_or(u64::MAX),
        ),
        // A failure or the deadline: the fallback stays.
        Err(_) => title.settle(None, 0),
    };
    settled
        .final_title()
        .filter(|chosen| **chosen != fallback)
        .cloned()
}

/// What a thread's agent runs on: its provider, model, effort and chosen speed (F15).
pub fn thread_run_settings(thread: &ThreadItem) -> RunSettings<String> {
    RunSettings {
        provider: provider(thread.provider),
        model: thread.model_name.clone(),
        effort: effort(thread.effort),
        speed: speed(thread.preferred_speed),
    }
}

/// What a duty agent's sessions run on (F7, F15): its provider and model, at the effort and
/// speed chosen when it was added. An agent saved before those could be chosen uses Settings'
/// defaults, like a new thread.
pub fn duty_run_settings(settings: &Settings, agent: &DutyAgentItem) -> RunSettings<String> {
    let defaults = &settings.providers;
    RunSettings {
        provider: provider(agent.provider),
        model: agent.model_name.clone(),
        effort: effort(agent.effort.unwrap_or(defaults.default_effort)),
        speed: speed(agent.preferred_speed.unwrap_or(defaults.default_speed)),
    }
}

pub fn provider(kind: ProviderKind) -> Provider {
    match kind {
        ProviderKind::Claude => Provider::Anthropic,
        ProviderKind::Codex => Provider::Codex,
        ProviderKind::Antigravity => Provider::Antigravity,
        ProviderKind::Kimi => Provider::KimiCode,
        ProviderKind::Custom(identifier) => Provider::Custom(identifier),
    }
}

fn effort(level: EffortLevel) -> Effort {
    match level {
        EffortLevel::Low => Effort::Low,
        EffortLevel::Medium => Effort::Medium,
        EffortLevel::High => Effort::High,
        EffortLevel::Max => Effort::Max,
    }
}

fn speed(mode: SpeedMode) -> Speed {
    match mode {
        SpeedMode::Regular => Speed::Regular,
        SpeedMode::Fast => Speed::Fast,
    }
}
