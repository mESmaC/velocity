//! Background titles for new build threads (SPEC.md F4, F15).

use std::path::PathBuf;
use std::time::Duration;

use velocity_acp::client::{ClientError, TitleRequest, request_title};
use velocity_acp::{Effort, Provider, RunSettings, Speed};
use velocity_core::thread_titles::{title_from_response, title_request_prompt};
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
                Duration::from_secs(30),
            )
        });
    TitlePlan {
        fallback,
        request,
        model_name,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TitleError {
    #[error(transparent)]
    Provider(#[from] ClientError),
    #[error("the model returned no usable title")]
    EmptyReply,
}

/// Each thread asks once, from its first message.
pub async fn resolve_title(request: TitleRequest) -> Result<String, TitleError> {
    let reply = request_title(request).await?;
    title_from_response(&reply.text).ok_or(TitleError::EmptyReply)
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

/// Explicit effort and speed choices on a duty agent take precedence over its defaults.
pub(crate) fn duty_run_settings(
    settings: &Settings,
    agent: &DutyAgentItem,
    role_effort: EffortLevel,
) -> RunSettings<String> {
    RunSettings {
        provider: provider(agent.provider),
        model: agent.model_name.clone(),
        effort: effort(agent.effort.unwrap_or(role_effort)),
        speed: speed(
            agent
                .preferred_speed
                .unwrap_or(settings.providers.default_speed),
        ),
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

pub(crate) fn effort(level: EffortLevel) -> Effort {
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
