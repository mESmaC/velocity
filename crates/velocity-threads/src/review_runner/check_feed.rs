//! Optional telemetry written by a project's fast check; never part of its verdict.

use std::io::Read;
use std::path::Path;

use velocity_rosen::RosenError;
use velocity_rosen::checker::{self, FailedCommand, Priority};

/// Capture only this run's telemetry, including a failing command's report. Old script
/// revisions visited during bisection must not reuse a newer script's leftover report.
pub(super) fn run_commands(
    folder: &Path,
    commands: &[String],
    priority: Priority,
) -> Result<(Option<FailedCommand>, String), RosenError> {
    let fresh = match std::fs::remove_file(folder.join("target/.velocity-checks/feed.txt")) {
        Ok(()) => true,
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    };
    let failed = checker::run_commands(folder, commands, priority)?;
    let suffix = if fresh {
        feed_suffix(folder)
    } else {
        String::new()
    };
    Ok((failed, suffix))
}

/// Keep a project-provided report small and on one feed line. Projects without the fast
/// script keep their existing feed, and a missing or unreadable report cannot fail a check.
fn feed_suffix(folder: &Path) -> String {
    let mut bytes = Vec::new();
    let read = std::fs::File::open(folder.join("target/.velocity-checks/feed.txt"))
        .and_then(|file| file.take(1_025).read_to_end(&mut bytes));
    if read.is_err() || bytes.len() > 1_024 {
        return String::new();
    }
    let line = String::from_utf8_lossy(&bytes)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if line.is_empty() {
        String::new()
    } else {
        format!(" · {line}")
    }
}
