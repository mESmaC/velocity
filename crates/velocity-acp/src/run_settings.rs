//! The provider, model, effort and speed an agent runs with (§3, F15).

/// The four providers Velocity runs agents from (§3). Each signs in with OAuth (F15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    Codex,
    Antigravity,
    KimiCode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
    Max,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Regular,
    Fast,
}

/// The speed an agent actually runs at. Fast exists only where the model has a fast mode,
/// which the caller reads from the agent's ACP config options rather than a fixed list.
pub fn effective_speed(requested: Speed, model_has_fast_mode: bool) -> Speed {
    if model_has_fast_mode {
        requested
    } else {
        Speed::Regular
    }
}

/// What a thread runs with. `Model` is the agent's own model identifier.
///
/// `speed` holds the speed you chose, not the one in effect: switching to a model without a
/// fast mode must not forget it, so that switching back runs at Fast again (F15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunSettings<Model> {
    pub provider: Provider,
    pub model: Model,
    pub effort: Effort,
    pub speed: Speed,
}

impl<Model> RunSettings<Model> {
    pub fn effective_speed(&self, model_has_fast_mode: bool) -> Speed {
        effective_speed(self.speed, model_has_fast_mode)
    }

    /// The same settings on another model. Provider, effort and the chosen speed are kept.
    pub fn with_model(self, model: Model) -> Self {
        RunSettings { model, ..self }
    }
}
