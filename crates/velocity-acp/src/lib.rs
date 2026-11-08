//! Velocity's ACP client and agent processes (SPEC.md §3.2).
//!
//! This crate starts with the agent runner's pure core: the settings an agent runs with
//! (F15), the answers that keep every agent unrestricted (Rule 1), and queued steering
//! (Rule 1, F4). Everything here is synchronous and free of I/O so that Charon and Aeneas
//! can translate it; `proofs/AgentRunner/` proves it against the design.

#![forbid(unsafe_code)]

pub mod permissions;
pub mod run_settings;
pub mod steering;

pub use permissions::{PermissionKind, SessionModeKind, choose_permission, choose_session_mode};
pub use run_settings::{Effort, Provider, RunSettings, Speed, effective_speed};
pub use steering::{Steering, SteeringEffect, SteeringInput};
