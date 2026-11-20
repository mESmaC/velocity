//! Requests to land a change under the project's Rosen policy.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use rosen_client::Endpoint;
use rosen_client::protocol::LandStatus;
use tokio::sync::RwLock;

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

/// One gate per server, by endpoint. Every land holds it shared while Rosen works on it;
/// replacing the server takes it alone (#515). Rosen's shutdown waits two seconds for
/// requests in flight, which a land that rebases and checks can outlast, so a server is
/// only stopped between lands, and a land that arrives meanwhile waits for the new one.
static LAND_GATES: LazyLock<Mutex<HashMap<String, Arc<RwLock<()>>>>> =
    LazyLock::new(Mutex::default);

pub(crate) fn land_gate(endpoint: &Endpoint) -> Arc<RwLock<()>> {
    let mut gates = LAND_GATES.lock().unwrap_or_else(|error| error.into_inner());
    Arc::clone(gates.entry(endpoint.to_string()).or_default())
}

/// Asks Rosen to land `change`.
pub async fn land(
    server: &ProjectServer,
    actor: &str,
    change: &str,
) -> Result<LandOutcome, RosenError> {
    let gate = land_gate(server.endpoint());
    let _landing = gate.read().await;
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
