//! Rosen owns channel state. This adapter retains only connection configuration.
use crate::{
    Entry, Error, Presence,
    channel::{DiscoveryIssue, entry},
};
use rosen_client::protocol::EventKind;
use rosen_client::protocol::channel::{AppendEntry, ChannelEvent, SendMessage};
use rosen_client::{Client, Endpoint};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::path::Path;

#[derive(Clone)]
pub(crate) struct Remote {
    pub(crate) endpoint: Endpoint,
    scope: String,
}

pub(crate) const RECORD_PREFIX: &str = "velocity-airc/v1:";

// The pinned Rosen protocol only has velocity_issue. Keep Rosen reports in its durable
// discovery ledger with agent authorship until the protocol supports a native field.
pub(crate) const DISCOVERY_PREFIX: &str = "velocity-airc/discovery/v1:";

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct RosenDiscovery {
    pub text: String,
    pub rosen_issue: String,
}

impl RosenDiscovery {
    fn encode(text: &str, title: String) -> Result<String, Error> {
        let discovery = Self {
            text: text.into(),
            rosen_issue: title,
        };
        Ok(format!(
            "{DISCOVERY_PREFIX}{}",
            serde_json::to_string(&discovery)?
        ))
    }
}

pub(crate) fn wire<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, Error> {
    Ok(serde_json::from_value(value)?)
}

impl Remote {
    pub(crate) fn new(app_data: &Path, endpoint: String) -> Self {
        Self {
            endpoint: Endpoint::new(endpoint),
            scope: member_scope(app_data),
        }
    }

    pub(crate) fn with_member_scope(mut self, scope: String) -> Result<Self, Error> {
        let hash = scope.strip_suffix('/').unwrap_or_default();
        if hash.len() != 16 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Invalid("Invalid launcher member scope".into()));
        }
        self.scope = scope;
        Ok(self)
    }

    pub(crate) fn member(&self, member: &str) -> String {
        if member.contains('/') || matches!(member, "you" | "velocity") {
            member.into()
        } else {
            format!("{}{member}", self.scope)
        }
    }

    pub(crate) fn local_member(&self, member: &str) -> String {
        member.strip_prefix(&self.scope).unwrap_or(member).into()
    }

    pub(crate) fn local_record(&self, record: &mut Entry) {
        record.member = self.local_member(&record.member);
        if let Some(claim) = &mut record.claim {
            claim.member = self.local_member(&claim.member);
        }
        if let Some(wait) = &mut record.dependency {
            wait.member = self.local_member(&wait.member);
            wait.holder = self.local_member(&wait.holder);
        }
    }

    pub(crate) fn for_project(app_data: &Path, project: &str) -> Result<Option<Self>, Error> {
        let Ok(identifier) = project.parse::<u32>() else {
            return Ok(None);
        };
        let server = velocity_rosen::ProjectServer::new(app_data, identifier);
        // Server configuration is the routing authority, never a probe whose failure
        // could split one Rosen channel into independent machine-local channels.
        if !server.data_dir().exists() && !configured_rosen(app_data, identifier)? {
            return Ok(None);
        }
        Ok(Some(Self::new(app_data, server.endpoint().as_str().into())))
    }

    pub(crate) fn run<T: Send>(
        &self,
        operation: impl AsyncFnOnce(Client) -> Result<T, Error> + Send,
    ) -> Result<T, Error> {
        // Channel's existing synchronous callers also run inside ACP's async runtime.
        // A separate thread avoids nested block_on and owns the connection's reader.
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(async {
                        let client = Client::connect(&self.endpoint).await?;
                        operation(client).await
                    })
                })
                .join()
                .map_err(|_| Error::Invalid("Rosen channel worker panicked".into()))?
        })
    }

    pub(crate) fn tip(&self) -> Result<u64, Error> {
        self.run(async |client| {
            let tip = client.status().await?.last_event;
            if tip != 0 {
                return Ok(tip);
            }
            Ok(crate::remote_events::read_events(&client, None, 0)
                .await?
                .last()
                .map_or(0, |record| record.sequence))
        })
    }

    pub(crate) fn event_head(&self) -> Result<velocity_rosen::EventHead, Error> {
        self.run(async |client| {
            velocity_rosen::EventHead::read(&client)
                .await
                .map_err(|error| Error::Invalid(error.to_string()))
        })
    }

    pub(crate) fn presence(&self) -> Result<Vec<Presence>, Error> {
        self.run(async |client| {
            Ok(client
                .channel_presence()
                .await?
                .into_iter()
                .map(|member| {
                    Presence::new(
                        self.local_member(&member.member),
                        member.nick,
                        member.left_at.is_none(),
                    )
                })
                .collect())
        })
    }

    pub(crate) fn join(&self, member: &str, requested: &str) -> Result<String, Error> {
        self.run(async |client| {
            let members = client.channel_presence().await?;
            if let Some(known) = members
                .iter()
                .find(|known| known.member == self.member(member) && known.left_at.is_none())
            {
                return Ok(known.nick.clone());
            }
            let base = crate::channel::nick(requested, 16);
            let machine = std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .unwrap_or_else(|_| "velocity".into());
            for suffix in 0..1000 {
                let ending = if suffix == 0 {
                    String::new()
                } else {
                    format!("-{suffix}")
                };
                let nick = format!(
                    "{}{}",
                    crate::channel::nick(&base, 16 - ending.len()),
                    ending
                );
                if matches!(nick.as_str(), "you" | "velocity") {
                    continue;
                }
                match client
                    .channel_join(&self.member(member), &nick, &machine)
                    .await
                {
                    Ok(member) => return Ok(member.nick),
                    Err(rosen_client::Error::Server(body))
                        if body.code == rosen_client::protocol::ErrorCode::Refused =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Err(Error::Invalid("No available AIRC nick".into()))
        })
    }

    pub(crate) fn leave(&self, member: &str) -> Result<(), Error> {
        self.run(async |client| {
            if client
                .channel_presence()
                .await?
                .iter()
                .any(|known| known.member == self.member(member) && known.left_at.is_none())
            {
                client.channel_leave(&self.member(member)).await?;
            }
            Ok(())
        })
    }

    pub(crate) fn message(
        &self,
        member: &str,
        to: Vec<String>,
        intent: &str,
        text: &str,
    ) -> Result<(), Error> {
        let message: SendMessage = wire(
            json!({"from":{"kind":"agent","member":self.member(member)},"to":to,"intent":intent,"text":text}),
        )?;
        self.run(async |client| {
            client.channel_send(message).await?;
            Ok(())
        })
    }

    pub(crate) fn comment(&self, text: &str) -> Result<Entry, Error> {
        self.run(async |client| {
            let mut recipients = Vec::new();
            let mut body = text;
            if let Some((nick, rest)) = text.split_once(':') {
                let nick = nick.trim();
                if client
                    .channel_presence()
                    .await?
                    .iter()
                    .any(|member| member.nick == nick && member.left_at.is_none())
                {
                    recipients.push(nick.to_owned());
                    body = rest.trim();
                } else if let Some(suggestion) = crate::comment::mistyped_nick(nick, &self.read(0)?)
                {
                    return Err(Error::Invalid(format!(
                        "Unknown nick `{nick}`. Did you mean `{suggestion}`?"
                    )));
                }
            }
            let message = wire(
                json!({"from":{"kind":"user"},"to":recipients,"intent":"inform","text":body}),
            )?;
            let sent = client.channel_send(message).await?;
            Ok(crate::remote_events::message_entry(&sent))
        })
    }

    pub(crate) fn ledger(
        &self,
        member: &str,
        ledger: &str,
        text: &str,
        issue: Option<DiscoveryIssue>,
    ) -> Result<(), Error> {
        crate::channel::require_text(text)?;
        let (text, velocity_issue) = match issue {
            Some(DiscoveryIssue::Velocity(title)) => (text.to_owned(), Some(title)),
            Some(DiscoveryIssue::Rosen(title)) => (RosenDiscovery::encode(text, title)?, None),
            None => (text.to_owned(), None),
        };
        let request: AppendEntry = wire(
            json!({"ledger":ledger,"author":{"kind":"agent","member":self.member(member)},"text":text,"velocity_issue":velocity_issue}),
        )?;
        self.run(async |client| {
            client.ledger_append(request).await?;
            Ok(())
        })
    }

    /// Runner metadata and observed Velocity events also live on the server. The
    /// versioned envelope distinguishes these records from human Past work entries.
    pub(crate) fn append_record(&self, record: &Entry) -> Result<Entry, Error> {
        let mut stored_record = record.clone();
        stored_record.member = self.member(&stored_record.member);
        if let Some(claim) = &mut stored_record.claim {
            claim.member = self.member(&claim.member);
        }
        if let Some(wait) = &mut stored_record.dependency {
            wait.member = self.member(&wait.member);
            wait.holder = self.member(&wait.holder);
        }
        let text = format!("{RECORD_PREFIX}{}", serde_json::to_string(&stored_record)?);
        let request: AppendEntry =
            wire(json!({"ledger":"past_work","author":{"kind":"user"},"text":text}))?;
        self.run(async |client| {
            // Ping does not wait for Rosen's subscription task to capture "now".
            // Replay from before the write so concurrent appends cannot be skipped.
            let after = client.status().await?.last_event;
            let stored = client.ledger_append(request).await?;
            let sequence = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                record_sequence(&client, after, stored.seq),
            )
            .await
            .map_err(|_| {
                Error::Invalid(format!(
                    "Rosen record {} is stored, but its event was not received within 30 seconds; \
                     read the channel before retrying",
                    stored.seq
                ))
            })??;
            let mut record = record.clone();
            record.sequence = sequence;
            Ok(record)
        })
    }

    pub(crate) fn release_held(&self, nick: &str) -> Result<(), Error> {
        self.run(async |client| {
            if let Some(member) = client
                .channel_presence()
                .await?
                .iter()
                .find(|member| member.nick == nick)
            {
                client.channel_release_held(&member.member).await?;
            }
            Ok(())
        })
    }

    pub(crate) fn set_mode(&self, nick: &str, mode: crate::MessageMode) -> Result<(), Error> {
        let mode = match mode {
            crate::MessageMode::Open => "open",
            crate::MessageMode::HeadsDown => "heads_down",
            crate::MessageMode::Quarantined => "quarantined",
        };
        self.run(async |client| {
            let member = client
                .channel_presence()
                .await?
                .into_iter()
                .find(|member| member.nick == nick)
                .ok_or_else(|| Error::Invalid("Unknown AIRC nick".into()))?;
            client
                .channel_set_mode(&member.member, wire(json!(mode))?)
                .await?;
            Ok(())
        })
    }

    pub(crate) fn record(&self, kind: &str, member: &str, nick: &str, text: &str) -> Entry {
        entry(&[], kind, member, nick, text)
    }
}

pub(crate) fn member_scope(app_data: &Path) -> String {
    let machine = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default();
    let location = std::fs::canonicalize(app_data).unwrap_or_else(|_| app_data.to_owned());
    let identity = format!("{machine}/{}", location.to_string_lossy())
        .replace('\\', "/")
        .to_lowercase();
    let hash = identity
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{hash:016x}/")
}

async fn record_sequence(client: &Client, after: u64, ledger_sequence: u64) -> Result<u64, Error> {
    // Subscribe after the append response: a forwarding daemon reports watermark
    // zero, so replay may fill the event buffer and block that response otherwise.
    let mut events = client.subscribe(Some(after)).await?;
    loop {
        let event = events
            .recv()
            .await
            .ok_or(rosen_client::Error::Disconnected)?;
        if event.kind != EventKind::Channel {
            continue;
        }
        let line: ChannelEvent = serde_json::from_value(event.detail)?;
        if matches!(line, ChannelEvent::Ledger { entry } if entry.seq == ledger_sequence) {
            return Ok(event.seq);
        }
    }
}

// Only needed before a project's server has its configuration directory. This reads
// project routing, never claims or presence; an unreachable Rosen project stays remote.
fn configured_rosen(app_data: &Path, identifier: u32) -> Result<bool, Error> {
    let bytes = match std::fs::read(app_data.join("state.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let state: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(state
        .get("projects")
        .and_then(|projects| projects.as_array())
        .into_iter()
        .flatten()
        .find(|project| project.get("id").and_then(|id| id.as_u64()) == Some(u64::from(identifier)))
        .is_some_and(|project| {
            project
                .get("version_control")
                .and_then(|value| value.as_str())
                .unwrap_or("rosen")
                == "rosen"
        }))
}
