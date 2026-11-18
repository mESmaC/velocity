//! Conflict notices are delivered only after the server's head reaches the workspace.

use crate::{ProjectServer, RosenError};

/// An old event must not resume an agent that already resolved or landed its change.
pub async fn has_open_conflicts(server: &ProjectServer, change: &str) -> Result<bool, RosenError> {
    let record = crate::connection::requests(server.endpoint())
        .await?
        .change_show(change)
        .await?
        .change;
    Ok(!record.state.is_closed() && !record.conflicts.is_empty())
}

/// Stable across retries and unique to the event, so the saved transcript acknowledges
/// this notice even if the follower stopped between sending it and removing its follow-up.
pub fn conflict_message(sequence: u64, paths: &[String]) -> String {
    let mut message = format!(
        "Rosen conflict event {sequence}: {} conflicted with work on the new base. Velocity has applied the server's current head to your workspace at a turn boundary. Read the files and combine both sides of any conflict markers (`ours` is your change, `theirs` is the incoming base). A file you already repaired may have no markers; verify it includes both changes.\n\nRun `rosen status` to see which conflicts remain, then `rosen resolve <path>` for each resolved file. In this workspace, resolve stores your edits before clearing the conflicts. Rosen refuses to resolve or land files with conflict markers. Finally name the work again with `rosen commit -m \"<what changed>\"`. You do not need to run `rosen rebase` for this notice.\n",
        paths.join(", ")
    );
    message.push_str(&velocity_core::rosen_checker::regeneration_instructions(
        paths,
    ));
    message
}

/// A generated-only rebase has no conflict records, but still needs the owner's work.
pub fn regeneration_message(sequence: u64, paths: &[String]) -> String {
    format!(
        "Rosen regeneration event {sequence}: the rebased workspace now holds the server's current head. Rosen kept the base's copies of these generated files because both sides edited them: {}. Regenerate them on the combined inputs before naming the work again; never combine their lines by hand. For other generated paths, use the project's generator.\n\n{}\nRun `rosen status`, resolve any remaining conflicts with `rosen resolve <path>`, then name the work again with `rosen commit -m \"<what changed>\"`.\n",
        paths.join(", "),
        velocity_core::rosen_checker::regeneration_instructions(paths)
    )
}
