//! Requests to land a change under the project's Rosen policy.

use rosen_client::protocol::LandStatus;

use crate::workspace::session;
use crate::{ProjectServer, RosenError};

/// What a land request came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandOutcome {
    Landed { commit: String },
    Refused(LandRefusal),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandRefusal {
    Regenerate { paths: Vec<String> },
    Conflict { paths: Vec<String>, message: String },
    Other { code: String, message: String },
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
            reason,
            message,
            paths,
        } => LandOutcome::Refused(match reason.as_str() {
            "regeneration_required" => LandRefusal::Regenerate { paths },
            "merge_conflict" | "conflicts" => LandRefusal::Conflict { paths, message },
            _ => LandRefusal::Other {
                code: reason,
                message,
            },
        }),
    })
}

/// Whether a queued notice still concerns a change that can be edited.
pub async fn is_open(server: &ProjectServer, change: &str) -> Result<bool, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    Ok(!client.change_show(change).await?.change.state.is_closed())
}
