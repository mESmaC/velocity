//! Velocity's own updates (SPEC.md F1 "Updates"): how this copy was installed, the update
//! state the footer panel shows, replacing a running portable executable, and supervising
//! the application so a fatal process exit resumes saved agent work (#397). Checking,
//! downloading and verifying releases go through the Tauri updater in `src-tauri`.
mod install_kind;
pub mod local_build;
mod portable;
mod status;
pub mod supervision;

pub use install_kind::{InstallKind, classify, detect};
pub use portable::{
    REPLACED_MARKER, UpdateError, relaunch, remove_previous, replace_running, wait_for_previous,
};
pub use status::{AvailableUpdate, UpdateAction, UpdatePhase, UpdateState, UpdateView};

/// The local wall-clock time a check finished, as the panel shows it.
pub fn local_clock() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

/// `time` as the local wall-clock time of day, such as `12:27`.
pub fn local_clock_at(time: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(time)
        .format("%H:%M")
        .to_string()
}
