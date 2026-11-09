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
use velocity_store::{EffortLevel, ProviderKind, Settings, SpeedMode};

use crate::launch::agent_launch;

/// How a new thread gets its title.
#[derive(Clone, Debug)]
pub struct TitlePlan {
    /// The message's first words: the title until the model answers, and after it fails.
    pub fallback: String,
    /// `None` when the title model's agent has no command yet (Antigravity, Q14): the thread
    /// keeps its fallback at once instead of waiting on a request that can't start.
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
    let request = agent_launch(providers.title_provider)
        .ok()
        .map(|(launch, _)| {
            let run_settings = RunSettings {
                provider: provider(providers.title_provider),
                model: providers.title_model.clone(),
                effort: effort(providers.title_effort),
                speed: speed(providers.title_speed),
            };
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
        model_name: providers.title_model.clone(),
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

fn provider(kind: ProviderKind) -> Provider {
    match kind {
        ProviderKind::Claude => Provider::Anthropic,
        ProviderKind::Codex => Provider::Codex,
        ProviderKind::Antigravity => Provider::Antigravity,
        ProviderKind::Kimi => Provider::KimiCode,
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
