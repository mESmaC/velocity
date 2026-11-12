//! Velocity's own updates (SPEC.md F1 "Updates"): how this copy was installed, the update
//! state the footer panel shows, and replacing a running portable executable. Checking,
//! downloading and verifying go through the Tauri updater in `src-tauri`; nothing here
//! talks to the network.
mod install_kind;
mod portable;
mod status;

pub use install_kind::{InstallKind, classify, detect};
pub use portable::{UpdateError, remove_previous, replace_running};
pub use status::{AvailableUpdate, UpdateAction, UpdatePhase, UpdateState, UpdateView};

/// The local wall-clock time a check finished, as the panel shows it.
pub fn local_clock() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}
