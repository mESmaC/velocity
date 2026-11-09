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

pub mod client;
pub mod paths;
pub mod permissions;
pub mod run_settings;
pub mod session_start;
pub mod steering;
pub mod terminal_output;
pub mod transcript;

pub use paths::{PathShape, resolve_path};
pub use permissions::{PermissionKind, SessionModeKind, choose_permission, choose_session_mode};
pub use run_settings::{Effort, Provider, RunSettings, Speed, effective_speed};
pub use session_start::{keep_first_by_name, mode_to_select};
pub use steering::{Steering, SteeringEffect, SteeringInput};
pub use terminal_output::retained_output_start;
pub use transcript::{Entry, EntryClass, MessageKey, Placement, Transcript};
