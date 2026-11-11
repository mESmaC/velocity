//! Velocity's ACP client and agent processes (SPEC.md §3.2).
//!
//! The pure core is synchronous and free of I/O:
//! - the agent runner (`proofs/AgentRunner/`): the settings an agent runs with (F15), the
//!   answers that keep every agent unrestricted (Rule 1), and queued steering (Rule 1, F4);
//! - the ACP client: the servers and requests that start a session, where `fs` and `terminal`
//!   paths point, how much terminal output to keep, and how `session/update` notifications
//!   become the transcript.
//!
//! `client` is the async edge around them: the agent process, the ACP connection, and the
//! `fs` and `terminal` capabilities.

#![forbid(unsafe_code)]

pub mod auth;
pub mod client;
pub mod concurrency;
pub mod paths;
pub mod permissions;
pub mod providers;
pub mod run_settings;
pub mod session_config;
pub mod session_start;
pub mod steering;
pub mod terminal_output;
pub mod tool_mutations;
pub mod transcript;
pub mod usage_limits;

pub use auth::{SignInKind, SignInMethod, choose_sign_in};
pub use client::{AgentSlashCommand, available_commands};
pub use paths::{PathShape, resolve_path};
pub use permissions::{PermissionKind, SessionModeKind, choose_permission, choose_session_mode};
pub use providers::{
    ProviderAgent, agent_command_label, agent_command_line, configured_agent, provider_agent,
};
pub use run_settings::{Effort, Provider, RunSettings, Speed, effective_speed};
pub use session_config::{
    ConfigCategory, ConfigChoices, ConfigOption, ConfigValue, choose_config_values, model_values,
    offers_fast_mode,
};
pub use session_start::{keep_first_by_name, mode_to_select};
pub use steering::{Steering, SteeringEffect, SteeringInput};
pub use terminal_output::retained_output_start;
pub use transcript::{Entry, EntryClass, MessageKey, Placement, Transcript};
pub use usage_limits::LimitReading;
