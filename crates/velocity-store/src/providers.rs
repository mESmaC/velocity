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

/// Why new work can't start on a provider (F15 Sign out).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProviderUnavailable {
    #[error(
        "You are signed out of {0}. Sign in again in Settings → Providers, or pick another model"
    )]
    SignedOut(String),
    #[error("No provider is signed in. Sign in to one in Settings → Providers")]
    NoneSignedIn,
    /// An agent you added was removed while work still pointed at it.
    #[error("That agent was removed from Settings → Providers. Pick another model")]
    Removed,
}

/// A model you hid from the pickers by clicking its chip (F15 Models).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct HiddenModel {
    pub provider: ProviderKind,
    pub model: String,
}

/// Facts about a provider, for its card and sign-in rows.
pub struct ProviderFacts {
    pub name: String,
    /// The agent it runs: "Claude Code".
    pub agent: String,
    pub sign_in_label: String,
    /// Where its OAuth page opens.
    pub site: String,
    /// The model a member runs when nothing has been read from the agent yet.
    pub fallback_model: String,
}

fn built_in_facts(provider: ProviderKind) -> Option<ProviderFacts> {
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
        ProviderKind::Custom(_) => return None,
    };
    Some(ProviderFacts {
        name: name.into(),
        agent: agent.into(),
        sign_in_label: sign_in_label.into(),
        site: site.into(),
        fallback_model: fallback_model.into(),
    })
}

/// A custom agent's card reads from what you entered: its name, and its own login in your
/// browser when it offers one (F15 Custom ACP agents). It has no built-in model, so until
/// its models are read the agent runs its own default.
fn custom_facts(name: &str) -> ProviderFacts {
    ProviderFacts {
        name: name.into(),
        agent: name.into(),
        sign_in_label: "Sign in".into(),
        site: "its sign-in page".into(),
        fallback_model: String::new(),
    }
}

/// An ACP agent command: the program, a path or a bare name on `PATH`, and its arguments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AgentCommand {
    pub program: String,
    #[serde(default)]
    pub arguments: Vec<String>,
}

impl AgentCommand {
    pub fn new(program: String, arguments: Vec<String>) -> Self {
        Self { program, arguments }
    }

    /// Reads `program arg arg` as typed in a card: words split on spaces, with double quotes
    /// around a word that holds spaces, such as `"C:\Program Files\agent\agent.exe" acp`.
    /// Backslashes are kept as typed, so Windows paths need no escaping. `None` when there
    /// is no program.
    pub fn parse(text: &str) -> Option<Self> {
        let mut words = split_command_words(text).into_iter();
        let program = words.next().filter(|program| !program.is_empty())?;
        Some(Self::new(program, words.collect()))
    }

    /// The command as one line, quoting words with spaces so `parse` reads it back.
    pub fn display(&self) -> String {
        std::iter::once(&self.program)
            .chain(&self.arguments)
            .map(|word| {
                if word.is_empty() || word.contains(char::is_whitespace) {
                    format!("\"{word}\"")
                } else {
                    word.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn split_command_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in text.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            _ if character.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            _ => {
                word.push(character);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// Why a command or custom agent from Settings → Providers was refused.
#[derive(Debug, thiserror::Error)]
pub enum AgentCommandError {
    #[error("Enter the command that starts the ACP agent")]
    MissingCommand,
    #[error("Name the agent")]
    MissingName,
    #[error("A provider named {0} already exists")]
    NameTaken(String),
    #[error("That agent was removed")]
    UnknownAgent,
    #[error("An agent you added has no built-in command to reset to")]
    NoBuiltIn,
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn parse_command(text: &str) -> Result<AgentCommand, AgentCommandError> {
    AgentCommand::parse(text).ok_or(AgentCommandError::MissingCommand)
}

/// Your command for a built-in provider's agent, in place of the built-in one (F15).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProviderCommand {
    pub provider: ProviderKind,
    pub command: AgentCommand,
}

/// An ACP agent you added (F15 Custom ACP agents). It holds no environment and no key: the
/// agent inherits your environment, as every agent does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CustomAgent {
    pub identifier: u32,
    pub name: String,
    pub command: AgentCommand,
}

impl Settings {
    /// Every provider in card order: the four built in, then the agents you added (F15).
    pub fn all_providers(&self) -> Vec<ProviderKind> {
        PROVIDERS
            .into_iter()
            .chain(
                self.providers
                    .custom_agents
                    .iter()
                    .map(|agent| ProviderKind::Custom(agent.identifier)),
            )
            .collect()
    }

    pub fn custom_agent(&self, identifier: u32) -> Option<&CustomAgent> {
        self.providers
            .custom_agents
            .iter()
            .find(|agent| agent.identifier == identifier)
    }

    /// Whether `provider` is one of the four or an agent you still have. A removed custom
    /// agent's threads keep its key, but no new work can start on it.
    pub fn provider_exists(&self, provider: ProviderKind) -> bool {
        match provider {
            ProviderKind::Custom(identifier) => self.custom_agent(identifier).is_some(),
            _ => true,
        }
    }

    pub fn provider_facts(&self, provider: ProviderKind) -> ProviderFacts {
        built_in_facts(provider).unwrap_or_else(|| match provider {
            ProviderKind::Custom(identifier) => match self.custom_agent(identifier) {
                Some(agent) => custom_facts(&agent.name),
                None => custom_facts("Removed agent"),
            },
            _ => custom_facts("Agent"),
        })
    }

    pub fn provider_name(&self, provider: ProviderKind) -> String {
        self.provider_facts(provider).name
    }

    /// The command you set for `provider`'s agent: your edit of a built-in one, or a custom
    /// agent's own. `None` means the built-in command.
    pub fn agent_command(&self, provider: ProviderKind) -> Option<&AgentCommand> {
        match provider {
            ProviderKind::Custom(identifier) => {
                self.custom_agent(identifier).map(|agent| &agent.command)
            }
            _ => self
                .providers
                .commands
                .iter()
                .find(|entry| entry.provider == provider)
                .map(|entry| &entry.command),
        }
    }

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
        self.provider_exists(provider)
            && !self.providers.signed_out.contains(&provider)
            && self
                .provider_catalog(provider)
                .is_none_or(|entry| entry.signed_in)
    }

    /// The providers new work can use, in card order (F15 Everywhere a model is chosen).
    pub fn usable_providers(&self) -> Vec<ProviderKind> {
        self.all_providers()
            .into_iter()
            .filter(|provider| self.provider_usable(*provider))
            .collect()
    }

    /// The provider new work starts on (F15 Sign out): the default for new threads while it
    /// is usable, else the first usable provider in card order. `None` when you are signed
    /// out of every provider, so nothing new can start.
    pub fn new_work_provider(&self) -> Option<ProviderKind> {
        let default = self.providers.default_provider;
        if self.provider_usable(default) {
            Some(default)
        } else {
            self.usable_providers().first().copied()
        }
    }

    /// The provider a dialog or record shows for new work. When no provider is usable it is
    /// the default; the launch itself is then refused by `ensure_new_work`.
    pub fn new_work_choice(&self) -> ProviderKind {
        self.new_work_provider()
            .unwrap_or(self.providers.default_provider)
    }

    /// Refuses new work on `provider` once you signed out of it in Velocity, or its agent
    /// reported no login. Running sessions never come through here, so they keep going.
    pub fn ensure_new_work(&self, provider: ProviderKind) -> Result<(), ProviderUnavailable> {
        if !self.provider_exists(provider) {
            Err(ProviderUnavailable::Removed)
        } else if self.provider_usable(provider) {
            Ok(())
        } else if self.usable_providers().is_empty() {
            Err(ProviderUnavailable::NoneSignedIn)
        } else {
            Err(ProviderUnavailable::SignedOut(self.provider_name(provider)))
        }
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
            .unwrap_or_else(|| self.provider_facts(provider).fallback_model)
    }
}

/// A provider's optional cap. Missing entries mean unlimited.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProviderConcurrency {
    pub provider: ProviderKind,
    pub maximum: u32,
}

impl Settings {
    pub fn provider_maximum(&self, provider: ProviderKind) -> Option<u32> {
        self.providers
            .concurrency
            .iter()
            .find(|entry| entry.provider == provider)
            .map(|entry| entry.maximum)
    }
}

/// A change from Settings → Providers or the home composer's model menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub enum ProviderEdit {
    MaxAgents {
        provider: ProviderKind,
        maximum: Option<u32>,
    },
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

    /// Sets the ACP agent command for `provider` from a card's command field, or with `None`
    /// returns a built-in provider to its built-in command (F15 Reset). A custom agent always
    /// has a command of its own, so Reset doesn't apply to it.
    pub fn set_provider_command(
        &mut self,
        provider: ProviderKind,
        command: Option<&str>,
    ) -> Result<bool, AgentCommandError> {
        let command = command.map(parse_command).transpose()?;
        let mut settings = self.state.settings.clone();
        let providers = &mut settings.providers;
        match (provider, command) {
            (ProviderKind::Custom(identifier), Some(command)) => {
                let agent = providers
                    .custom_agents
                    .iter_mut()
                    .find(|agent| agent.identifier == identifier)
                    .ok_or(AgentCommandError::UnknownAgent)?;
                agent.command = command;
            }
            (ProviderKind::Custom(_), None) => return Err(AgentCommandError::NoBuiltIn),
            (_, command) => {
                providers
                    .commands
                    .retain(|entry| entry.provider != provider);
                if let Some(command) = command {
                    providers
                        .commands
                        .push(ProviderCommand { provider, command });
                }
            }
        }
        Ok(self.replace_settings(settings)?)
    }

    /// Add an ACP agent (F15 Custom ACP agents): its own card after the four, read over ACP
    /// like a built-in provider. Returns the new provider.
    pub fn add_custom_agent(
        &mut self,
        name: &str,
        command: &str,
    ) -> Result<ProviderKind, AgentCommandError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AgentCommandError::MissingName);
        }
        let command = parse_command(command)?;
        let mut settings = self.state.settings.clone();
        let agents = &mut settings.providers.custom_agents;
        if agents
            .iter()
            .any(|agent| agent.name.eq_ignore_ascii_case(name))
            || PROVIDERS.into_iter().any(|provider| {
                built_in_facts(provider).is_some_and(|facts| facts.name.eq_ignore_ascii_case(name))
            })
        {
            return Err(AgentCommandError::NameTaken(name.into()));
        }
        // Identifiers are never reused, so a removed agent's threads never point at a new one.
        let identifier = settings.providers.next_custom_agent.max(
            agents
                .iter()
                .map(|agent| agent.identifier.saturating_add(1))
                .max()
                .unwrap_or(1),
        );
        settings.providers.next_custom_agent = identifier.saturating_add(1);
        settings.providers.custom_agents.push(CustomAgent {
            identifier,
            name: name.into(),
            command,
        });
        self.replace_settings(settings)?;
        Ok(ProviderKind::Custom(identifier))
    }

    /// Removes an agent you added, with what Velocity read from it. Its threads keep their
    /// record; new work can't start on it. A default that pointed at it returns to the
    /// built-in default.
    pub fn remove_custom_agent(&mut self, identifier: u32) -> Result<bool, AgentCommandError> {
        let provider = ProviderKind::Custom(identifier);
        let mut settings = self.state.settings.clone();
        let defaults = Settings::default().providers;
        let providers = &mut settings.providers;
        let before = providers.custom_agents.len();
        providers
            .custom_agents
            .retain(|agent| agent.identifier != identifier);
        if providers.custom_agents.len() == before {
            return Err(AgentCommandError::UnknownAgent);
        }
        providers.catalog.retain(|entry| entry.provider != provider);
        providers.signed_out.retain(|entry| *entry != provider);
        providers
            .hidden_models
            .retain(|hidden| hidden.provider != provider);
        if providers.default_provider == provider {
            providers.default_provider = defaults.default_provider;
            providers.default_model = defaults.default_model;
        }
        if providers.title_provider == provider {
            providers.title_provider = defaults.title_provider;
            providers.title_model = defaults.title_model;
        }
        Ok(self.replace_settings(settings)?)
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
        ProviderEdit::MaxAgents { provider, maximum } => set_maximum(providers, provider, maximum),
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

fn set_maximum(
    providers: &mut crate::ProviderSettings,
    provider: ProviderKind,
    maximum: Option<u32>,
) {
    providers
        .concurrency
        .retain(|entry| entry.provider != provider);
    if let Some(maximum) = maximum {
        providers.concurrency.push(ProviderConcurrency {
            provider,
            maximum: maximum.max(1),
        });
    }
}
