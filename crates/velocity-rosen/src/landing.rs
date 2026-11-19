//! Requests to land a change under the project's Rosen policy.

use rosen_client::protocol::LandStatus;

use crate::workspace::session;
use crate::{ProjectServer, RosenError};

/// What a land request came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandOutcome {
    Landed {
        commit: String,
    },
    /// Refused, with Rosen's stable code (`unapproved`, `receipt_stale`, `needs_recheck`,
    /// `merge_conflict`, ...) and its advice.
    Refused {
        code: String,
        message: String,
    },
}

/// Asks Rosen to land `change`.
pub async fn land(
    server: &ProjectServer,
    actor: &str,
    change: &str,
) -> Result<LandOutcome, RosenError> {
    let answer = session(server, actor).await?.land(change).await?;
    Ok(match answer.outcome {
        LandStatus::Landed { commit, .. } | LandStatus::AlreadyLanded { commit } => {
            LandOutcome::Landed {
                commit: commit.to_string(),
            }
        }
        LandStatus::Refused {
            reason, message, ..
        } => LandOutcome::Refused {
            code: reason,
            message,
        },
    })
}

/// Whether a queued notice still concerns a change that can be edited.
pub async fn is_open(server: &ProjectServer, change: &str) -> Result<bool, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    Ok(!client.change_show(change).await?.change.state.is_closed())
}
