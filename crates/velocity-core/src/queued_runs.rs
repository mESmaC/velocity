//! Persisted queued runs (`queue_run`, #516, #536).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Maximum number of finished runs retained per thread for `run_status` lookup and recovery.
pub const MAX_RUN_HISTORY: usize = 50;

/// The Rosen run-queue class an agent chose for a queued command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueuedRunClass {
    #[default]
    Build,
    Landing,
    Verify,
}

impl QueuedRunClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Landing => "landing",
            Self::Verify => "verify",
        }
    }
}

/// Lifecycle phase of a queued run (`queue_run`, #536).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueuedRunPhase {
    /// Accepted during an active model turn; starts once the turn ends.
    WaitingForTurnEnd,
    /// Waiting in Rosen's run queue, a light Rust slot, or the memory gate.
    #[default]
    Queued,
    /// Turn acquired and the child command is executing.
    Running,
    /// Command exited with status 0.
    Completed,
    /// Command exited non-zero or failed to start.
    Failed,
    /// Timed out waiting for a run-queue turn.
    TimedOut,
    /// Cancelled via `run_cancel` or session shutdown.
    Cancelled,
}

impl QueuedRunPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WaitingForTurnEnd => "waiting_for_turn_end",
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::TimedOut | Self::Cancelled
        )
    }
}

/// One queued command persisted on its owning thread for restart recovery and `run_status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedRunRecord {
    pub id: String,
    pub program: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Explicit environment variables passed to the queued command (for example a registered
    /// `[generated.commands]` command such as `UPDATE_FIXTURES=1`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Working directory, relative to the thread's workspace or absolute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default)]
    pub class: QueuedRunClass,
    /// Full log file path outside the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    /// Last observed lifecycle phase.
    #[serde(default)]
    pub phase: QueuedRunPhase,
    /// Reason the run is waiting or queued, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_reason: Option<String>,
    /// Rosen change ID checked by this run, when in a Rosen workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    /// Rosen workspace head (tree ID) checked by this run, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
    /// Process exit code when the command exited normally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Human-readable exit status (e.g. `exit code 0`, `timed out`, `cancelled`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<String>,
    /// Outcome summary recorded when the run reached a terminal phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// Whether this run's terminal result was delivered to the agent in a prompt.
    #[serde(default)]
    pub delivered: bool,
    /// UTC Unix seconds when the run was accepted into the queue (#619).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<u64>,
    /// Milliseconds spent waiting in the queue before execution started (#619).
    #[serde(default)]
    pub wait_ms: u64,
    /// Milliseconds spent executing the command (#619).
    #[serde(default)]
    pub run_ms: u64,
}

impl QueuedRunRecord {
    pub fn new(
        id: String,
        program: String,
        arguments: Vec<String>,
        directory: Option<String>,
        class: QueuedRunClass,
    ) -> Self {
        Self {
            id,
            program,
            arguments,
            env: BTreeMap::new(),
            directory,
            class,
            log: None,
            phase: QueuedRunPhase::Queued,
            queue_reason: None,
            change: None,
            head: None,
            exit_code: None,
            exit_status: None,
            result: None,
            delivered: false,
            queued_at: None,
            wait_ms: 0,
            run_ms: 0,
        }
    }

    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// The command and its arguments joined for display.
    pub fn command_line(&self) -> String {
        let mut parts: Vec<String> = self
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        parts.push(self.program.clone());
        parts.extend(self.arguments.iter().cloned());
        parts.join(" ")
    }

    /// Display title matching `QueuedRun`'s tool step row.
    pub fn title(&self) -> String {
        let joined = format!("rosen run -- {}", self.command_line())
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let mut characters = joined.chars();
        let mut title: String = characters.by_ref().take(240).collect();
        if characters.next().is_some() {
            title.push('…');
        }
        title
    }

    /// One-line description of an interrupted run naming its ID, command, last phase and log path.
    pub fn summary_line(&self) -> String {
        let command = self.command_line();
        let location = match &self.directory {
            Some(directory) if !directory.is_empty() => {
                format!("class: {}, directory: `{directory}`", self.class.as_str())
            }
            _ => format!("class: {}", self.class.as_str()),
        };
        let phase = match &self.queue_reason {
            Some(reason) if !reason.trim().is_empty() && !self.phase.is_terminal() => {
                format!("{} ({})", self.phase.as_str(), reason.trim())
            }
            _ => self.phase.as_str().to_owned(),
        };
        let log = self.log.as_deref().unwrap_or("none");
        format!(
            "- `{}` · `{command}` ({location}) · last phase: {phase} · log: `{log}`",
            self.id
        )
    }

    /// One-line description of a result recorded before Velocity shut down.
    pub fn recorded_result_line(&self) -> String {
        let command = self.command_line();
        let status = self
            .exit_status
            .as_deref()
            .filter(|status| !status.trim().is_empty())
            .map_or_else(
                || self.phase.as_str().to_owned(),
                |status| format!("{} ({status})", self.phase.as_str()),
            );
        let log = self.log.as_deref().unwrap_or("none");
        let header = format!("- `{}` · `{command}` · {status} · log: `{log}`", self.id);
        match &self.result {
            Some(result) if !result.trim().is_empty() => {
                let excerpt = crate::text_budget::prompt_excerpt(
                    result.trim(),
                    1024,
                    "\n[Result excerpt. Use `run_status` or read the full log above.]",
                );
                format!("{header}\n  {}", excerpt.replace('\n', "\n  "))
            }
            _ => header,
        }
    }
}

fn pre_shutdown_results_section(results: &[QueuedRunRecord]) -> String {
    if results.is_empty() {
        "Results recorded before shutdown: none.".to_owned()
    } else {
        let list = results
            .iter()
            .map(QueuedRunRecord::recorded_result_line)
            .collect::<Vec<_>>()
            .join("\n");
        format!("Results recorded before shutdown:\n{list}")
    }
}

/// Prompt notice delivered after Velocity restarts and requeues interrupted runs (#536).
pub fn restarted_runs_notice(
    interrupted: &[QueuedRunRecord],
    pre_shutdown_results: &[QueuedRunRecord],
) -> String {
    let list = interrupted
        .iter()
        .map(QueuedRunRecord::summary_line)
        .collect::<Vec<_>>()
        .join("\n");
    let recorded = pre_shutdown_results_section(pre_shutdown_results);
    format!(
        "[Velocity] Velocity restarted while these queued runs were outstanding and requeued \
         them:\n{list}\n\n{recorded}\n\nReview the requeued runs' results above (or call \
         `run_status` with a run ID) and continue where you left off."
    )
}

/// Prompt delivered when restarting persisted queued runs is impossible, including any
/// pre-shutdown recorded results.
pub fn lost_runs_prompt_with_results(
    runs: &[QueuedRunRecord],
    pre_shutdown_results: &[QueuedRunRecord],
    reason: Option<&str>,
) -> String {
    let suffix = match reason {
        Some(reason) if !reason.trim().is_empty() => format!(" ({})", reason.trim()),
        _ => String::new(),
    };
    let list = runs
        .iter()
        .map(QueuedRunRecord::summary_line)
        .collect::<Vec<_>>()
        .join("\n");
    let recorded = pre_shutdown_results_section(pre_shutdown_results);
    format!(
        "[Velocity] Velocity restarted while these queued runs were outstanding, and could not \
         start them again{suffix}:\n{list}\n\n{recorded}\n\nQueue them again with `queue_run` \
         if you still need their results, then continue where you left off."
    )
}

/// Prompt delivered to a thread when restarting its persisted queued runs is impossible.
pub fn lost_runs_prompt(runs: &[QueuedRunRecord], reason: Option<&str>) -> String {
    lost_runs_prompt_with_results(runs, &[], reason)
}

/// Prompt delivered after a restart when no interrupted runs remain to requeue, while still
/// naming any pre-shutdown recorded results or stating explicitly that there were none.
pub fn continue_after_restart_prompt(
    interrupted: &[QueuedRunRecord],
    pre_shutdown_results: &[QueuedRunRecord],
) -> String {
    if !interrupted.is_empty() {
        return restarted_runs_notice(interrupted, pre_shutdown_results);
    }
    let recorded = pre_shutdown_results_section(pre_shutdown_results);
    format!(
        "[Velocity] Velocity was closed while you were working and has started again. Your \
         worktree, claims and transcript are kept.\n\nInterrupted queued runs: none.\n{recorded}\n\n\
         Any direct shell command that was running in a terminal was stopped: run it again if \
         you still need its result. Continue where you left off."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Risk (#536): restart notices must list each interrupted run's ID, command, last phase and
    // log path, include pre-shutdown results when present, and state explicitly when there were none.
    #[test]
    fn restart_notice_names_interrupted_runs_and_pre_shutdown_results() {
        let mut interrupted = QueuedRunRecord::new(
            "velocity-run-abc".into(),
            "cargo".into(),
            vec!["check".into(), "-p".into(), "velocity-acp".into()],
            None,
            QueuedRunClass::Build,
        );
        interrupted.phase = QueuedRunPhase::Running;
        interrupted.log = Some("C:\\temp\\velocity-run-abc.log".into());

        let without_results = restarted_runs_notice(&[interrupted.clone()], &[]);
        assert!(
            without_results.contains("velocity-run-abc"),
            "{without_results}"
        );
        assert!(
            without_results.contains("cargo check -p velocity-acp"),
            "{without_results}"
        );
        assert!(
            without_results.contains("last phase: running"),
            "{without_results}"
        );
        assert!(
            without_results.contains("C:\\temp\\velocity-run-abc.log"),
            "{without_results}"
        );
        assert!(
            without_results.contains("Results recorded before shutdown: none."),
            "{without_results}"
        );

        let mut finished = QueuedRunRecord::new(
            "velocity-run-done".into(),
            "pnpm".into(),
            vec!["-C".into(), "ui".into(), "check".into()],
            None,
            QueuedRunClass::Build,
        );
        finished.phase = QueuedRunPhase::Completed;
        finished.exit_code = Some(0);
        finished.exit_status = Some("exit code 0".into());
        finished.log = Some("C:\\temp\\velocity-run-done.log".into());
        finished.result = Some("[Velocity] Passed · rosen run -- pnpm -C ui check".into());

        let with_results = restarted_runs_notice(&[interrupted], &[finished]);
        assert!(
            with_results.contains("Results recorded before shutdown:\n- `velocity-run-done`"),
            "{with_results}"
        );
        assert!(with_results.contains("exit code 0"), "{with_results}");
        assert!(
            with_results.contains("C:\\temp\\velocity-run-done.log"),
            "{with_results}"
        );
    }
}
