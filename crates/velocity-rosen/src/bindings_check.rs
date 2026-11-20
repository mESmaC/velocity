//! The IPC bindings check a land waits on (#503). A change that registers a Tauri command,
//! event or constant without regenerating `bindings.ts` merges cleanly, so Rosen lands it
//! and the base's UI build breaks. Velocity therefore runs the project's
//! `export-bindings --check` on the change's merge result first, as a light run in a folder
//! of its own beside the project's workspaces: the folder keeps its build from one change
//! to the next, so only what a change touched compiles.

use std::path::Path;
use std::process::Output;

use velocity_core::rosen_checker::{BINDINGS, BINDINGS_EXPORTER};

use crate::{ProjectServer, RosenError, agent_run, checker};

/// `cargo` arguments, run from the root of the tree. The manifest names the package, so
/// the check does not depend on what a project calls its Tauri crate.
const CHECK: &[&str] = &[
    "run",
    "--manifest-path",
    "src-tauri/Cargo.toml",
    "--bin",
    "export-bindings",
    "--",
    "--check",
];
/// How many lines of a run that gave no verdict the reason keeps.
const REASON_LINES: usize = 4;

/// What the check found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingsVerdict {
    /// The bindings match the Rust code, or the tree has none to check.
    Current,
    /// The exporter wrote a different file: the change goes back to its author.
    Stale,
    /// The check could not decide, and why. Not a verdict on the change.
    Unchecked(String),
}

/// Stages `change`'s merge onto the base beside `folder`. `false` when the merge needs its
/// author's repair, which Rosen's own refusal reports: there is then nothing to check.
pub async fn stage(
    server: &ProjectServer,
    change: &str,
    folder: &Path,
) -> Result<bool, RosenError> {
    let merge = checker::stage_merge_result(server, change, folder).await?;
    Ok(merge.conflicts.is_empty())
}

/// Brings `folder` to the staged merge result and checks its bindings, building into
/// `target` (blocking).
pub fn check_staged(folder: &Path, target: &Path) -> BindingsVerdict {
    if let Err(error) = checker::apply_staged(folder) {
        return BindingsVerdict::Unchecked(error.to_string());
    }
    if !folder.join(BINDINGS_EXPORTER).is_file() {
        return BindingsVerdict::Current;
    }
    let bindings = folder.join(BINDINGS);
    let committed = unix_text(&bindings);
    let output = match agent_run::light_output(folder, target, "cargo", CHECK) {
        Ok(output) => output,
        Err(error) => return BindingsVerdict::Unchecked(error.to_string()),
    };
    if output.status.success() {
        return BindingsVerdict::Current;
    }
    // `--check` regenerates the file before it fails, so a different file is the verdict.
    // The same file means the exporter never ran: a compile error or a missing tool, which
    // the integration build or the machine's owner answers for, not this check.
    if unix_text(&bindings) == committed {
        BindingsVerdict::Unchecked(reason(&output))
    } else {
        BindingsVerdict::Stale
    }
}

/// The file's text without carriage returns, as the exporter compares it. A missing file
/// reads as empty, so a tree that never had bindings is stale once the exporter writes them.
fn unix_text(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .replace("\r\n", "\n")
}

/// The end of what the run printed, on one line.
fn reason(output: &Output) -> String {
    let printed = [&output.stdout[..], &output.stderr[..]].concat();
    let printed = String::from_utf8_lossy(&printed);
    let lines: Vec<&str> = printed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.is_empty() {
        return format!("`cargo {}` {}", CHECK.join(" "), output.status);
    }
    lines[lines.len().saturating_sub(REASON_LINES)..].join(" · ")
}
