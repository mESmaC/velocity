//! Persisted outstanding queued runs (`queue_run`, #516).

use serde::{Deserialize, Serialize};

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

/// One queued command persisted on its owning thread until it completes or the session stops.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedRunRecord {
    pub id: String,
    pub program: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    /// Working directory, relative to the thread's workspace or absolute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    #[serde(default)]
    pub class: QueuedRunClass,
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
            directory,
            class,
        }
    }

    /// Display title matching `QueuedRun`'s tool step row.
    pub fn title(&self) -> String {
        let joined = format!("rosen run -- {} {}", self.program, self.arguments.join(" "))
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

    /// One-line description for prompts reporting lost or restarted runs.
    pub fn summary_line(&self) -> String {
        let command = if self.arguments.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.arguments.join(" "))
        };
        match &self.directory {
            Some(directory) if !directory.is_empty() => format!(
                "- `{command}` (class: {}, directory: `{directory}`)",
                self.class.as_str()
            ),
            _ => format!("- `{command}` (class: {})", self.class.as_str()),
        }
    }
}

/// Prompt delivered to a thread when restarting its persisted queued runs is impossible.
pub fn lost_runs_prompt(runs: &[QueuedRunRecord], reason: Option<&str>) -> String {
    let suffix = match reason {
        Some(reason) if !reason.trim().is_empty() => format!(" ({})", reason.trim()),
        _ => String::new(),
    };
    let list = runs
        .iter()
        .map(QueuedRunRecord::summary_line)
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "[Velocity] Velocity restarted while these queued runs were outstanding, and could not \
         start them again{suffix}:\n{list}\n\nQueue them again with `queue_run` if you still need \
         their results, then continue where you left off."
    )
}

/// Prompt line added after persisted queued runs finish following a Velocity restart.
pub const RESTARTED_RUNS_NOTICE: &str = "[Velocity] Velocity restarted while your queued runs \
    were outstanding and started them again. Review the results above and continue where you left off.";
