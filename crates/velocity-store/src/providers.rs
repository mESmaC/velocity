//! Providers (SPEC.md F15): what Velocity last read from each provider's agent, the sign-outs
//! and hidden models you chose, and the read models drawn from them: Settings → Providers,
//! the composer model menu and the footer limit meter.
//!
//! The catalog is saved so pickers, the model pill and the meter are right at startup, before
//! the agents are asked again. It holds no token: sign-in state lives in each agent's own
//! credential store, and the catalog only records what the agent last said.

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{EffortLevel, ProviderKind, Settings, SpeedMode, StateStore, StoreError};

/// Every provider, in the order cards and menus list them (F15).
pub const PROVIDERS: [ProviderKind; 4] = [
    ProviderKind::Claude,
    ProviderKind::Codex,
    ProviderKind::Antigravity,
    ProviderKind::Kimi,
];

/// Now in Unix seconds, for reset times. A clock before 1970 reads as 0.
pub fn unix_now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u32::try_from(elapsed.as_secs()).unwrap_or(u32::MAX)
        })
}

/// One model a provider's agent offers, as last read from it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProviderModel {
    /// The agent's own value for the model.
    pub id: String,
    pub name: String,
    /// Whether the agent offers a fast mode on it (F15 Speed).
    #[serde(default)]
    pub fast: bool,
}

impl ProviderModel {
    pub fn new(id: String, name: String, fast: bool) -> Self {
        Self { id, name, fast }
    }
}

/// The share of one subscription window used, as last read (F15 Limit sources).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProviderLimit {
    pub percent_used: u8,
    /// `5-hour`, `daily`, `weekly`.
    pub window: String,
    /// When the window resets, in Unix seconds.
    #[serde(default)]
    pub resets_at: Option<u32>,
}

impl ProviderLimit {
    pub fn new(percent_used: u8, window: String, resets_at: Option<u32>) -> Self {
        Self {
            percent_used,
            window,
            resets_at,
        }
    }
}

/// What Velocity last read from one provider's agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProviderCatalog {
    pub provider: ProviderKind,
    pub signed_in: bool,
    #[serde(default)]
    pub plan: Option<String>,
    #[serde(default)]
    pub models: Vec<ProviderModel>,
    #[serde(default)]
    pub limit: Option<ProviderLimit>,
}

impl ProviderCatalog {
    pub fn new(provider: ProviderKind) -> Self {
        Self {
            provider,
            signed_in: false,
            plan: None,
            models: Vec::new(),
            limit: None,
        }
    }
}

/// A model you hid from the pickers by clicking its chip (F15 Models).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct HiddenModel {
    pub provider: ProviderKind,
    pub model: String,
}

/// Static facts about a provider, for its card and sign-in rows.
pub struct ProviderFacts {
    pub name: &'static str,
    /// The agent it runs: "Claude Code".
    pub agent: &'static str,
    pub sign_in_label: &'static str,
    /// Where its OAuth page opens.
    pub site: &'static str,
    /// The model a member runs when nothing has been read from the agent yet.
    pub fallback_model: &'static str,
}

pub fn provider_facts(provider: ProviderKind) -> ProviderFacts {
    let (name, agent, sign_in_label, site, fallback_model) = match provider {
        ProviderKind::Claude => (
            "Anthropic",
            "Claude Code",
            "Sign in with Claude",
            "claude.ai",
            "Opus 5.5",
        ),
        ProviderKind::Codex => (
            "Codex",
            "Codex CLI",
            "Sign in with ChatGPT",
            "chatgpt.com",
            "GPT-5.5 Codex",
        ),
        ProviderKind::Antigravity => (
            "Antigravity",
            "Antigravity",
            "Sign in with Google",
            "accounts.google.com",
            "Gemini 3 Pro",
        ),
        ProviderKind::Kimi => (
            "Kimi Code",
            "Kimi CLI",
            "Sign in with Kimi",
            "kimi.com",
            "Kimi K2",
        ),
    };
    ProviderFacts {
        name,
        agent,
        sign_in_label,
        site,
        fallback_model,
    }
}

impl Settings {
    pub fn provider_catalog(&self, provider: ProviderKind) -> Option<&ProviderCatalog> {
        self.providers
            .catalog
            .iter()
            .find(|entry| entry.provider == provider)
    }

    /// Whether new work can use `provider`: its agent last reported a login and you haven't
    /// signed out of it in Velocity. A provider never read yet counts as usable, so a fresh
    /// install launches as before until its agents have answered.
    pub fn provider_usable(&self, provider: ProviderKind) -> bool {
        !self.providers.signed_out.contains(&provider)
            && self
                .provider_catalog(provider)
                .is_none_or(|entry| entry.signed_in)
    }

    /// The providers new work can use, in card order (F15 Everywhere a model is chosen).
    pub fn usable_providers(&self) -> Vec<ProviderKind> {
        PROVIDERS
            .into_iter()
            .filter(|provider| self.provider_usable(*provider))
            .collect()
    }

    /// The models the pickers list for `provider`: what its agent offers, minus the ones you
    /// hid.
    pub fn visible_models(&self, provider: ProviderKind) -> Vec<&ProviderModel> {
        self.provider_catalog(provider)
            .map(|entry| {
                entry
                    .models
                    .iter()
                    .filter(|model| !self.model_hidden(provider, &model.name))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn model_hidden(&self, provider: ProviderKind, model: &str) -> bool {
        self.providers
            .hidden_models
            .iter()
            .any(|hidden| hidden.provider == provider && hidden.model == model)
    }

    /// Whether the agent offers a fast mode on `model`. A model never read is taken to have
    /// none, so Fast is never shown where it can't run.
    pub fn model_has_fast_mode(&self, provider: ProviderKind, model: &str) -> bool {
        self.provider_catalog(provider).is_some_and(|entry| {
            entry
                .models
                .iter()
                .any(|known| known.fast && (known.name == model || known.id == model))
        })
    }

    /// The model new work on `provider` runs: Settings' default model on the default
    /// provider, else the provider's first visible model, else a built-in name the agent
    /// matches against its own list (F15).
    pub fn provider_model(&self, provider: ProviderKind) -> String {
        let providers = &self.providers;
        providers
            .default_model
            .clone()
            .filter(|name| provider == providers.default_provider && !name.trim().is_empty())
            .or_else(|| {
                self.visible_models(provider)
                    .first()
                    .map(|model| model.name.clone())
            })
            .unwrap_or_else(|| provider_facts(provider).fallback_model.into())
    }
}

/// A change from Settings → Providers or the home composer's model menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum ProviderEdit {
    /// Show or hide a model chip in the pickers.
    ShowModel {
        provider: ProviderKind,
        model: String,
        shown: bool,
    },
    DefaultModel {
        provider: ProviderKind,
        model: String,
    },
    DefaultEffort(EffortLevel),
    DefaultSpeed(SpeedMode),
    TitleModel {
        provider: ProviderKind,
        model: String,
    },
    TitleEffort(EffortLevel),
    TitleSpeed(SpeedMode),
    /// Back to GPT-6 Luna · Low · Fast (F15 Thread titles).
    TitleReset,
}

impl StateStore {
    /// Records what a provider's agent reported on its last probe. Returns whether anything
    /// changed.
    pub fn record_provider_probe(
        &mut self,
        provider: ProviderKind,
        signed_in: bool,
        models: Vec<ProviderModel>,
    ) -> Result<bool, StoreError> {
        self.update_catalog(provider, |entry| {
            entry.signed_in = signed_in;
            // A signed-out agent lists nothing; the last list stays for when it returns.
            if signed_in && !models.is_empty() {
                entry.models = models;
            }
        })
    }

    /// Records a limit reading and the plan it named.
    pub fn record_provider_limit(
        &mut self,
        provider: ProviderKind,
        limit: Option<ProviderLimit>,
        plan: Option<String>,
    ) -> Result<bool, StoreError> {
        self.update_catalog(provider, |entry| {
            entry.limit = limit;
            if plan.is_some() {
                entry.plan = plan;
            }
        })
    }

    /// Sign out (F15): new work can't use the provider; running agents keep their session.
    /// Signing in again clears it.
    pub fn set_provider_signed_out(
        &mut self,
        provider: ProviderKind,
        signed_out: bool,
    ) -> Result<bool, StoreError> {
        let mut settings = self.state.settings.clone();
        let list = &mut settings.providers.signed_out;
        list.retain(|entry| *entry != provider);
        if signed_out {
            list.push(provider);
        }
        self.replace_settings(settings)
    }

    pub fn edit_providers(&mut self, edit: ProviderEdit) -> Result<bool, StoreError> {
        let mut settings = self.state.settings.clone();
        apply_edit(&mut settings, edit);
        self.replace_settings(settings)
    }

    fn update_catalog(
        &mut self,
        provider: ProviderKind,
        change: impl FnOnce(&mut ProviderCatalog),
    ) -> Result<bool, StoreError> {
        let mut settings = self.state.settings.clone();
        let catalog = &mut settings.providers.catalog;
        let index = match catalog.iter().position(|entry| entry.provider == provider) {
            Some(index) => index,
            None => {
                catalog.push(ProviderCatalog::new(provider));
                catalog.len() - 1
            }
        };
        change(&mut catalog[index]);
        self.replace_settings(settings)
    }

    fn replace_settings(&mut self, settings: Settings) -> Result<bool, StoreError> {
        if self.state.settings == settings {
            return Ok(false);
        }
        self.set_settings(settings)?;
        Ok(true)
    }
}

fn apply_edit(settings: &mut Settings, edit: ProviderEdit) {
    let providers = &mut settings.providers;
    match edit {
        ProviderEdit::ShowModel {
            provider,
            model,
            shown,
        } => {
            providers
                .hidden_models
                .retain(|hidden| !(hidden.provider == provider && hidden.model == model));
            if !shown {
                providers
                    .hidden_models
                    .push(HiddenModel { provider, model });
            }
        }
        ProviderEdit::DefaultModel { provider, model } => {
            providers.default_provider = provider;
            providers.default_model = Some(model);
        }
        ProviderEdit::DefaultEffort(effort) => providers.default_effort = effort,
        ProviderEdit::DefaultSpeed(speed) => providers.default_speed = speed,
        ProviderEdit::TitleModel { provider, model } => {
            providers.title_provider = provider;
            providers.title_model = model;
        }
        ProviderEdit::TitleEffort(effort) => providers.title_effort = effort,
        ProviderEdit::TitleSpeed(speed) => providers.title_speed = speed,
        ProviderEdit::TitleReset => {
            let defaults = Settings::default().providers;
            providers.title_provider = defaults.title_provider;
            providers.title_model = defaults.title_model;
            providers.title_effort = defaults.title_effort;
            providers.title_speed = defaults.title_speed;
        }
    }
}
