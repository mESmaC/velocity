use crate::InstallKind;

/// A newer release the last check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: String,
    pub notes: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdatePhase {
    /// No check has run since Velocity started.
    NotChecked,
    Checking,
    UpToDate,
    Available,
    Downloading {
        received: u64,
        total: Option<u64>,
    },
    /// Verified and handed to the installer (or swapped in); Velocity is restarting.
    Installing,
    Failed(String),
}

/// What the footer Updates panel knows. `src-tauri` drives the transitions from the
/// updater's results; [`UpdateState::view`] turns it into what the panel shows.
#[derive(Clone, Debug)]
pub struct UpdateState {
    pub current_version: String,
    pub install_kind: InstallKind,
    pub phase: UpdatePhase,
    pub available: Option<AvailableUpdate>,
    /// When the last check finished, as the clock shows it ("22:15").
    pub checked_at: Option<String>,
}

impl UpdateState {
    pub fn new(current_version: &str, install_kind: InstallKind) -> Self {
        Self {
            current_version: current_version.to_owned(),
            install_kind,
            phase: UpdatePhase::NotChecked,
            available: None,
            checked_at: None,
        }
    }

    /// Checks never start over a download or an install already under way.
    pub fn can_check(&self) -> bool {
        !matches!(
            self.phase,
            UpdatePhase::Checking | UpdatePhase::Downloading { .. } | UpdatePhase::Installing
        )
    }

    pub fn checked(&mut self, found: Option<AvailableUpdate>, at: &str) {
        self.phase = if found.is_some() {
            UpdatePhase::Available
        } else {
            UpdatePhase::UpToDate
        };
        self.available = found;
        self.checked_at = Some(at.to_owned());
    }

    pub fn failed(&mut self, message: &str) {
        self.phase = UpdatePhase::Failed(message.to_owned());
    }

    /// Reports `true` when the whole percent shown changed, so progress is published at
    /// most a hundred times per download.
    pub fn downloaded(&mut self, chunk: u64, total: Option<u64>) -> bool {
        let before = self.percent();
        let received = match self.phase {
            UpdatePhase::Downloading { received, .. } => received,
            _ => 0,
        };
        self.phase = UpdatePhase::Downloading {
            received: received.saturating_add(chunk),
            total,
        };
        before != self.percent()
    }

    fn percent(&self) -> Option<u8> {
        match self.phase {
            UpdatePhase::Downloading {
                received,
                total: Some(total),
            } if total > 0 => u8::try_from(received.saturating_mul(100) / total)
                .ok()
                .map(|percent| percent.min(100)),
            _ => None,
        }
    }
}

/// The one action the panel offers.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum UpdateAction {
    None,
    Check,
    Install,
}

/// The Updates panel and the footer badge, ready to render.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct UpdateView {
    pub headline: String,
    pub detail: String,
    /// The release notes of the available version, as published.
    pub notes: String,
    /// Shown above the install button: what restarting does to working agents.
    pub warning: String,
    pub install_label: String,
    pub action: UpdateAction,
    pub action_label: String,
    /// 0–100 while downloading with a known size.
    pub progress: Option<u8>,
    /// The footer button's dot: a newer version is ready to install.
    pub badge: bool,
}

impl UpdateState {
    pub fn view(&self, working_agents: usize) -> UpdateView {
        let (headline, detail, action) = self.phase_text();
        let offering = matches!(self.phase, UpdatePhase::Available);
        let downloading = matches!(self.phase, UpdatePhase::Downloading { .. });
        UpdateView {
            headline,
            detail: detail.trim().to_owned(),
            notes: match &self.available {
                Some(update) if offering || downloading => update.notes.trim().to_owned(),
                _ => String::new(),
            },
            warning: if offering {
                restart_warning(working_agents)
            } else {
                String::new()
            },
            install_label: self.install_kind.label().to_owned(),
            action_label: match action {
                UpdateAction::None => String::new(),
                UpdateAction::Check => "Check for updates".to_owned(),
                UpdateAction::Install => "Restart to update".to_owned(),
            },
            action,
            progress: self.percent(),
            badge: offering,
        }
    }

    fn phase_text(&self) -> (String, String, UpdateAction) {
        let next = self
            .available
            .as_ref()
            .map(|update| update.version.as_str())
            .unwrap_or_default();
        let current = &self.current_version;
        let checked = self
            .checked_at
            .as_ref()
            .map(|at| format!("Checked at {at}."))
            .unwrap_or_default();
        match &self.phase {
            UpdatePhase::NotChecked => (
                format!("Velocity {current}"),
                "Not checked yet.".to_owned(),
                UpdateAction::Check,
            ),
            UpdatePhase::Checking => (
                "Checking for updates…".to_owned(),
                String::new(),
                UpdateAction::None,
            ),
            UpdatePhase::UpToDate => (
                format!("Velocity {current} is up to date"),
                checked,
                UpdateAction::Check,
            ),
            UpdatePhase::Available => (
                format!("Velocity {next} is available"),
                format!("You have {current}. {checked}"),
                UpdateAction::Install,
            ),
            UpdatePhase::Downloading { .. } => (
                format!("Downloading Velocity {next}…"),
                String::new(),
                UpdateAction::None,
            ),
            UpdatePhase::Installing => (
                format!("Installing Velocity {next}…"),
                "Velocity restarts when the install finishes.".to_owned(),
                UpdateAction::None,
            ),
            UpdatePhase::Failed(message) => (
                "The update didn't finish".to_owned(),
                message.clone(),
                UpdateAction::Check,
            ),
        }
    }
}

/// Restarting ends every running turn (§3.2 Resume after a restart): worktrees and
/// transcripts are kept, and each cut-off turn continues by itself once Velocity is back.
fn restart_warning(working_agents: usize) -> String {
    match working_agents {
        0 => String::new(),
        1 => "1 agent is working. Restarting stops its turn; it continues where it left off, in the same worktree, when Velocity starts again.".to_owned(),
        count => format!(
            "{count} agents are working. Restarting stops their turns; each continues where it left off, in the same worktree, when Velocity starts again."
        ),
    }
}
