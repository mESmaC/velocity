//! Answers that keep every agent unrestricted (Rule 1, §3.2 Tool access).
//!
//! Velocity has no approval state: each permission request is answered as soon as it
//! arrives, by these pure functions, and is never shown to you.

/// ACP's `PermissionOptionKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// The index of the option to answer a `session/request_permission` with: the first
/// allow-always option, or the first allow-once option when the agent offers no
/// allow-always option. Never a reject option. `None` only when the agent offers no allow
/// option at all.
pub fn choose_permission(options: &[PermissionKind]) -> Option<usize> {
    let mut first_allow_once = None;
    let mut index = 0;
    while index < options.len() {
        match options[index] {
            PermissionKind::AllowAlways => return Some(index),
            PermissionKind::AllowOnce => {
                if first_allow_once.is_none() {
                    first_allow_once = Some(index);
                }
            }
            PermissionKind::RejectOnce | PermissionKind::RejectAlways => {}
        }
        index += 1;
    }
    first_allow_once
}

/// A session mode an agent offers, as the provider layer classifies it. `Bypass` skips
/// the agent's own permission checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionModeKind {
    Bypass,
    Other,
}

/// The index of the mode to select with `session/set_mode` when a session starts: the
/// first bypass mode, or `None` when the agent offers none.
pub fn choose_session_mode(modes: &[SessionModeKind]) -> Option<usize> {
    let mut index = 0;
    while index < modes.len() {
        if let SessionModeKind::Bypass = modes[index] {
            return Some(index);
        }
        index += 1;
    }
    None
}
