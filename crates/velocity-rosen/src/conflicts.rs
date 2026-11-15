//! Conflict notices are delivered only after the server's head reaches the workspace.

use rosen_client::Client;

use crate::{ProjectServer, RosenError};

/// An old event must not resume an agent that already resolved or landed its change.
pub async fn has_open_conflicts(server: &ProjectServer, change: &str) -> Result<bool, RosenError> {
    let record = Client::connect(server.endpoint())
        .await?
        .change_show(change)
        .await?
        .change;
    Ok(!record.state.is_closed() && !record.conflicts.is_empty())
}

/// Stable across retries and unique to the event, so the saved transcript acknowledges
/// this notice even if the follower stopped between sending it and removing its follow-up.
pub fn conflict_message(sequence: u64, paths: &[String]) -> String {
    format!(
        "Rosen conflict event {sequence}: {} conflicted with work on the new base. Velocity has applied the server's current head to your workspace at a turn boundary. Read the files and combine both sides of any conflict markers (`ours` is your change, `theirs` is the incoming base). A file you already repaired may have no markers; verify it includes both changes.\n\nRun `rosen status` to see which conflicts remain, then `rosen resolve <path>` for each resolved file. In this workspace, resolve stores your edits before clearing the conflicts. Rosen refuses to resolve or land files with conflict markers. Finally name the work again with `rosen commit -m \"<what changed>\"`. You do not need to run `rosen rebase` for this notice.\n",
        paths.join(", ")
    )
}
