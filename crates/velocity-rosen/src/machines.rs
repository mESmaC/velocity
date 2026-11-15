//! One Rosen credential enrolls a machine in version control and Velocity dispatch.
use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use rosen_client::protocol::{CreateJoinKey, MachineRecord, MachineRole, MachineStatus};
use serde::{Deserialize, Serialize};

use crate::{ProjectServer, RosenError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Agent,
    Checker,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Checker => "checker",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct Machine {
    pub id: String,
    pub name: String,
    pub role: Role,
    pub status: String,
    pub last_contact: String,
    pub running_agents: Vec<String>,
    pub dispatch_ready: bool,
}

#[derive(Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct MachinesView {
    pub machines: Vec<Machine>,
    pub listener: Option<String>,
    pub recent_dispatch: Vec<String>,
}

pub async fn view(server: &ProjectServer) -> Result<MachinesView, RosenError> {
    let client = server.connect().await?;
    let messages = client.channel_messages(None, 0).await?;
    let mut recent_dispatch: Vec<String> = messages
        .messages
        .into_iter()
        .rev()
        .filter(|message| {
            message.from.starts_with("velocity-machine-") && message.text.starts_with("Dispatch ")
        })
        .take(10)
        .map(|message| message.text)
        .collect();
    recent_dispatch.reverse();
    Ok(MachinesView {
        machines: list(server).await?,
        listener: client.status().await?.listen,
        recent_dispatch,
    })
}

impl Machine {
    fn from_record(record: MachineRecord, registered: bool) -> Self {
        Self {
            id: record.id,
            name: record.name,
            role: if record.role == MachineRole::Checker {
                Role::Checker
            } else {
                Role::Agent
            },
            status: match record.status {
                MachineStatus::Active => "active",
                MachineStatus::Pending => "pending",
                MachineStatus::Revoked => "revoked",
            }
            .into(),
            last_contact: record.last_contact.to_string(),
            running_agents: record.running_agents,
            dispatch_ready: registered
                && record.status == MachineStatus::Active
                && now().saturating_sub(record.last_contact) <= 90,
        }
    }
}

/// A stable nick on the authenticated project channel; no second dispatch secret exists.
pub fn dispatch_name(machine: &str) -> String {
    format!("velocity-machine-{machine}")
}

/// Enrollment pins the server before transmitting the join secret. Rosen keeps the
/// resulting credential in the OS credential store; Velocity saves only the machine ID.
pub async fn enroll(server: &ProjectServer, key: &str, name: &str) -> Result<String, RosenError> {
    let credential = rosen_client::enrollment::join(server.data_dir(), key, name)
        .await
        .map_err(invalid)?;
    Ok(credential.machine_id)
}

pub async fn list(server: &ProjectServer) -> Result<Vec<Machine>, RosenError> {
    let client = server.connect().await?;
    let presence = client.channel_presence().await?;
    Ok(client
        .machine_list()
        .await?
        .into_iter()
        .map(|record| {
            let registered = presence.iter().any(|member| {
                member.member == dispatch_name(&record.id) && member.left_at.is_none()
            });
            Machine::from_record(record, registered)
        })
        .collect())
}

pub async fn approve(server: &ProjectServer, machine: &str) -> Result<(), RosenError> {
    server.connect().await?.machine_approve(machine).await?;
    Ok(())
}

pub async fn revoke(server: &ProjectServer, machine: &str) -> Result<(), RosenError> {
    let client = server.connect().await?;
    // Revoke the credential first. Even if the presence update fails, neither channel
    // requests nor Rosen requests from this machine can pass authentication again.
    client.machine_revoke(machine).await?;
    client.channel_leave(&dispatch_name(machine)).await?;
    Ok(())
}

pub async fn create_key(
    server: &ProjectServer,
    address: String,
    expires_in_seconds: u32,
    uses: u32,
    role: Role,
) -> Result<String, RosenError> {
    if address.trim().is_empty() || expires_in_seconds == 0 || uses == 0 {
        return Err(RosenError::Machine(
            "An address, positive expiry and use limit are required".into(),
        ));
    }
    let client = server.connect().await?;
    if client.status().await?.listen.is_none() {
        return Err(RosenError::Machine(
            "Enable the project's machine listener first".into(),
        ));
    }
    // Rosen owns this wire type and has no constructor. Deserialize its request rather
    // than coupling Velocity to a full literal of another crate's record.
    let request: CreateJoinKey = serde_json::from_value(serde_json::json!({
        "address": address.trim(), "expires_at": now() + u64::from(expires_in_seconds),
        "use_limit": uses, "role": role,
    }))
    .map_err(invalid)?;
    Ok(client.create_join_key(request).await?)
}

pub async fn enable_listener(server: &ProjectServer, address: &str) -> Result<(), RosenError> {
    let socket: SocketAddr = address.parse().map_err(invalid)?;
    if socket.port() == 0 {
        return Err(RosenError::Machine(
            "Choose a fixed machine listener port".into(),
        ));
    }
    let client = server.connect().await?;
    if client.status().await?.listen.as_deref() == Some(address) {
        return Ok(());
    }
    let path = server.data_dir().join("velocity-listen");
    let previous = match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(invalid(error)),
    };
    std::fs::write(&path, address).map_err(invalid)?;
    server.stop().await;
    if let Err(error) = server.connect().await {
        // An occupied or invalid bind address must not leave the project unable to
        // start, which would also prevent correcting it through the native command.
        match previous {
            Some(bytes) => std::fs::write(&path, bytes).map_err(invalid)?,
            None => std::fs::remove_file(&path).map_err(invalid)?,
        }
        let _ = server.connect().await;
        return Err(error);
    }
    Ok(())
}

pub(crate) fn invalid(error: impl std::fmt::Display) -> RosenError {
    RosenError::Machine(error.to_string())
}

pub(crate) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
