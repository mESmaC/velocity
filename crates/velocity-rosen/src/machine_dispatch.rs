//! Dispatch uses Rosen's authenticated channel, so revocation has a single authority.
use rosen_client::protocol::channel::{Author, Intent, SendMessage};
use serde::{Deserialize, Serialize};

use crate::machines::{Role, dispatch_name, invalid};
use crate::{ProjectServer, RosenError};

pub const JOB_PREFIX: &str = "velocity-dispatch-v1:";
pub const CONTROLLER: &str = "velocity-dispatch";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub prompt: String,
    pub role: Role,
}

impl Job {
    pub fn new(prompt: String, role: Role) -> Self {
        Self { prompt, role }
    }

    pub fn decode(text: &str) -> Result<Option<Self>, RosenError> {
        text.strip_prefix(JOB_PREFIX)
            .map(|json| serde_json::from_str(json).map_err(invalid))
            .transpose()
    }
}

/// Returns the durable channel message number, also the worker's deduplication key.
pub async fn dispatch(
    server: &ProjectServer,
    machine: &str,
    prompt: String,
) -> Result<String, RosenError> {
    if prompt.trim().is_empty() {
        return Err(invalid("Enter work to dispatch"));
    }
    let target = crate::machines::list(server)
        .await?
        .into_iter()
        .find(|row| row.id == machine && row.dispatch_ready)
        .ok_or_else(|| invalid("The machine is not active and registered for dispatch"))?;
    let text = format!(
        "{JOB_PREFIX}{}",
        serde_json::to_string(&Job::new(prompt, target.role)).map_err(invalid)?
    );
    let message: SendMessage = serde_json::from_value(serde_json::json!({
        "from": Author::User, "to": [dispatch_name(machine)], "intent": Intent::Request, "text": text,
    })).map_err(invalid)?;
    let client = server.connect().await?;
    client
        .channel_join(CONTROLLER, CONTROLLER, "controller")
        .await?;
    let sent = client.channel_send(message).await?;
    Ok(sent.seq.to_string())
}

pub async fn agent_session(
    server: &ProjectServer,
    agent: &str,
) -> Result<rosen_client::Session, RosenError> {
    crate::workspace::session(server, agent).await
}

pub fn existing_change(root: &std::path::Path) -> Result<String, RosenError> {
    Ok(rosen_client::snapshot::WorkspaceMarker::read(root)
        .map_err(invalid)?
        .change
        .to_string())
}

pub async fn change_closed(server: &ProjectServer, change: &str) -> Result<bool, RosenError> {
    Ok(server
        .connect()
        .await?
        .change_show(change)
        .await?
        .change
        .state
        .is_closed())
}

pub async fn report(
    server: &ProjectServer,
    machine: &str,
    sequence: u64,
    result: &str,
) -> Result<(), RosenError> {
    let member = dispatch_name(machine);
    let message: SendMessage = serde_json::from_value(serde_json::json!({
        "from": Author::Agent { member }, "to": [CONTROLLER], "intent": Intent::Inform,
        "text": format!("Dispatch {sequence}: {result}"),
    }))
    .map_err(invalid)?;
    server.connect().await?.channel_send(message).await?;
    Ok(())
}

/// Reads through the cache daemon, which forwards every call to the authenticated
/// upstream. An offline cache can never admit new work from a stale local roster.
pub async fn register(server: &ProjectServer, machine: &str) -> Result<Role, RosenError> {
    let client = server.connect().await?;
    let record = client
        .machine_list()
        .await?
        .into_iter()
        .find(|row| row.id == machine)
        .ok_or_else(|| invalid("The enrolled machine is missing from Rosen"))?;
    if record.status != rosen_client::protocol::MachineStatus::Active {
        return Err(invalid("The machine is pending or revoked"));
    }
    let name = dispatch_name(machine);
    client.channel_join(&name, &name, machine).await?;
    Ok(
        if record.role == rosen_client::protocol::MachineRole::Checker {
            Role::Checker
        } else {
            Role::Agent
        },
    )
}
