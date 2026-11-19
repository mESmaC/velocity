//! Catch a project's base up to its Git mirror when another machine's server landed
//! there (#501, Rosen R1 #416), and adopt a ported mirror tip (Rosen #396).
//!
//! The owner works on two machines, each with its own server mirroring to one remote.
//! The idle machine's base falls behind and its next push is refused as
//! non-fast-forward. Rosen's catch-up fetches the mirror and, when the base is an
//! ancestor of its tip, moves the base there and rebases open changes, keeping the
//! server's database. Velocity never re-imports or replaces a database to catch up.

use rosen_client::protocol::{CatchUpStatus, ErrorCode};
use velocity_core::rosen_mirror::MirrorDivergence;

use crate::{ClientError, ProjectServer, RosenError};

/// What catching up from the mirror did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatchUp {
    /// Nothing to catch up here: Rosen's reason, such as a project with no mirror.
    NotApplicable(String),
    /// The mirror branch is at the base.
    UpToDate,
    /// The base already contains the mirror tip; the next push sends the rest.
    Ahead,
    /// The base moved forward to the mirror tip and open changes were rebased onto it.
    FastForwarded(FastForward),
    /// Both sides have commits the other lacks. Nothing moved.
    Diverged(MirrorDivergence),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FastForward {
    pub new_base: String,
    /// The mirror commits the base now holds.
    pub commits: usize,
    /// Open changes moved onto the new base.
    pub rebased: usize,
    /// Of those, the changes that now hold conflicts.
    pub conflicted: usize,
    /// Changes Rosen could not move; each rebases when it lands.
    pub rebase_errors: Vec<String>,
}

/// Rosen refuses catch-up for a project with no mirror.
const NO_MIRROR: &str = "no mirror configured";
/// Rosen takes catch-up only on the server's local endpoint. A project whose server
/// Velocity reaches over the network is caught up on that server's own machine.
const NOT_LOCAL: &str = "operator command";

/// Fetches the project's mirror and fast-forwards the base to its tip when that is
/// possible. Diverged histories come back as [`CatchUp::Diverged`], untouched.
pub async fn catch_up_from_mirror(server: &ProjectServer) -> Result<CatchUp, RosenError> {
    let client = server.connect().await?;
    let answer = match client.mirror_catch_up().await {
        Ok(answer) => answer,
        Err(ClientError::Server(body)) if body.code == ErrorCode::Refused => {
            return refusal(body.message);
        }
        Err(error) => return Err(error.into()),
    };
    Ok(match answer.status {
        CatchUpStatus::UpToDate => CatchUp::UpToDate,
        CatchUpStatus::Ahead => CatchUp::Ahead,
        CatchUpStatus::FastForwarded => CatchUp::FastForwarded(FastForward {
            new_base: answer.new_base.to_hex(),
            commits: answer.commits.len(),
            rebased: answer.rebased.len(),
            conflicted: answer.conflicted.len(),
            rebase_errors: answer
                .rebase_errors
                .into_iter()
                .map(|failure| format!("{}: {}", failure.change, failure.error))
                .collect(),
        }),
    })
}

/// Sorts Rosen's refusal into a divergence, nothing to do, or an error to report.
fn refusal(message: String) -> Result<CatchUp, RosenError> {
    if let Some(divergence) = MirrorDivergence::from_refusal(&message) {
        return Ok(CatchUp::Diverged(divergence));
    }
    if message.contains(NO_MIRROR) || message.contains(NOT_LOCAL) {
        return Ok(CatchUp::NotApplicable(message));
    }
    Err(RosenError::MirrorRefused(message))
}

/// Adopts the mirror tip `commit` as a parent of the base, for a mirror whose edits
/// were already ported into Rosen (Rosen #396). Rosen refuses when they were not.
/// Returns the new base.
pub async fn adopt_mirror_tip(server: &ProjectServer, commit: &str) -> Result<String, RosenError> {
    let commit = crate::policy::object(commit)?;
    let client = server.connect().await?;
    match client.mirror_adopt(commit).await {
        Ok(adoption) => Ok(adoption.new_base.to_hex()),
        Err(ClientError::Server(body)) if body.code == ErrorCode::Refused => {
            Err(RosenError::MirrorRefused(body.message))
        }
        Err(error) => Err(error.into()),
    }
}
