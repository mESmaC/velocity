//! Rosen owns channel state. This adapter retains only connection configuration.
use crate::{
    Entry, Error, Presence,
    channel::{DiscoveryIssue, entry},
};
use rosen_client::Endpoint;
use rosen_client::protocol::channel::{AppendEntry, SendMessage};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::cell::Cell;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use velocity_rosen::connection::Connection;

thread_local! {
    static CONNECT_DURATION: Cell<Duration> = const { Cell::new(Duration::ZERO) };
}

pub(crate) fn reset_connect_duration() {
    CONNECT_DURATION.with(|cell| cell.set(Duration::ZERO));
}

pub(crate) fn take_connect_duration() -> Duration {
    CONNECT_DURATION.with(|cell| cell.replace(Duration::ZERO))
}

pub(crate) fn record_connect_duration(elapsed: Duration) {
    CONNECT_DURATION.with(|cell| cell.set(cell.get().saturating_add(elapsed)));
}

/// Dispatches an async Rosen operation onto the single shared Rosen runtime without
/// spawning an OS thread or creating a Tokio runtime (#555).
/// Callers must never invoke synchronous `Remote` methods from inside `future`.
pub(crate) fn run_on_shared_runtime<T: Send + 'static>(
    future: impl Future<Output = Result<T, Error>> + Send + 'static,
) -> Result<T, Error> {
    let handle = velocity_rosen::connection::shared_handle()?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    handle.spawn(async move {
        let _ = sender.send(future.await);
    });
    receiver
        .recv()
        .map_err(|_| Error::Invalid("Rosen channel worker stopped".into()))?
}

pub(crate) fn is_transport_error(error: &Error) -> bool {
    matches!(
        error,
        Error::Rosen(rosen_client::Error::Disconnected | rosen_client::Error::Io(_))
    )
}

#[derive(Clone)]
pub(crate) struct Remote {
    pub(crate) endpoint: Endpoint,
    pub(crate) scope: String,
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

    /// Routes a project channel to Rosen when the caller's store config marks it Rosen
    /// or its Rosen data folder already exists on disk. Never parses `state.json` (#555).
    pub(crate) fn for_project(
        app_data: &Path,
        project: &str,
        rosen: Option<bool>,
    ) -> Result<Option<Self>, Error> {
        let Ok(identifier) = project.parse::<u32>() else {
            return Ok(None);
        };
        let server = velocity_rosen::ProjectServer::new(app_data, identifier);
        let use_rosen = rosen.unwrap_or(false) || server.data_dir().exists();
        if !use_rosen {
            return Ok(None);
        }
        Ok(Some(Self::new(app_data, server.endpoint().as_str().into())))
    }

    pub(crate) fn run<T, F, Fut>(&self, operation: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Connection>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, Error>> + Send + 'static,
    {
        let endpoint = self.endpoint.clone();
        let (result, connect_elapsed) = run_on_shared_runtime(async move {
            let (connection, connect_elapsed) =
                velocity_rosen::connection::requests_timed(&endpoint).await?;
            let result = operation(Arc::clone(&connection)).await;
            if let Err(ref error) = result
                && is_transport_error(error)
            {
                velocity_rosen::connection::invalidate(&endpoint, &connection).await;
            }
            Ok((result, connect_elapsed))
        })?;
        record_connect_duration(connect_elapsed);
        result
    }

    pub(crate) fn tip(&self) -> Result<u64, Error> {
        let tip = self.run(|client| async move { Ok(client.status().await?.last_event) })?;
        if tip != 0 {
            return Ok(tip);
        }
        Ok(self.snapshot()?.last().map_or(0, |record| record.sequence))
    }

    pub(crate) fn event_head(&self) -> Result<velocity_rosen::EventHead, Error> {
        self.run(|client| async move {
            velocity_rosen::EventHead::read(&client)
                .await
                .map_err(|error| Error::Invalid(error.to_string()))
        })
    }

    pub(crate) fn presence(&self) -> Result<Vec<Presence>, Error> {
        let scope = self.scope.clone();
        self.run(move |client| async move {
            Ok(client
                .channel_presence()
                .await?
                .into_iter()
                .map(|member| {
                    let local = member
                        .member
                        .strip_prefix(&scope)
                        .unwrap_or(&member.member)
                        .to_owned();
                    Presence::new(local, member.nick, member.left_at.is_none())
                })
                .collect())
        })
    }

    pub(crate) fn join(&self, member: &str, requested: &str) -> Result<String, Error> {
        let scoped = self.member(member);
        let base = crate::channel::nick(requested, crate::channel::NICK_LIMIT);
        let machine = std::env::var("COMPUTERNAME")
            .or_else(|_| std::env::var("HOSTNAME"))
            .unwrap_or_else(|_| "velocity".into());
        self.run(move |client| async move {
            let members = client.channel_presence().await?;
            if let Some(known) = members
                .iter()
                .find(|known| known.member == scoped && known.left_at.is_none())
            {
                return Ok(known.nick.clone());
            }
            for suffix in 0..1000 {
                let ending = if suffix == 0 {
                    String::new()
                } else {
                    format!("-{suffix}")
                };
                let nick = format!(
                    "{}{}",
                    crate::channel::nick(
                        &base,
                        crate::channel::NICK_LIMIT.saturating_sub(ending.len())
                    ),
                    ending
                );
                if matches!(nick.as_str(), "you" | "velocity") {
                    continue;
                }
                match client.channel_join(&scoped, &nick, &machine).await {
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
        let scoped = self.member(member);
        self.run(move |client| async move {
            if client
                .channel_presence()
                .await?
                .iter()
                .any(|known| known.member == scoped && known.left_at.is_none())
            {
                client.channel_leave(&scoped).await?;
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
        self.run(move |client| async move {
            client.channel_send(message).await?;
            Ok(())
        })
    }

    pub(crate) fn comment(&self, text: &str) -> Result<Entry, Error> {
        let mut recipients = Vec::new();
        let mut body = text;
        if let Some((nick, rest)) = text.split_once(':') {
            let nick = nick.trim();
            let active = self
                .presence()?
                .into_iter()
                .any(|member| member.joined && member.nick == nick);
            if active {
                recipients.push(nick.to_owned());
                body = rest.trim();
            } else if let Some(suggestion) = crate::comment::mistyped_nick(nick, &self.snapshot()?)
            {
                return Err(Error::Invalid(format!(
                    "Unknown nick `{nick}`. Did you mean `{suggestion}`?"
                )));
            }
        }
        let message: SendMessage =
            wire(json!({"from":{"kind":"user"},"to":recipients,"intent":"inform","text":body}))?;
        self.run(move |client| async move {
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
        self.run(move |client| async move {
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
        let stored_seq =
            self.run(move |client| async move { Ok(client.ledger_append(request).await?.seq) })?;
        let sequence = self.wait_ledger_sequence(stored_seq)?;
        let mut record = record.clone();
        record.sequence = sequence;
        Ok(record)
    }

    pub(crate) fn release_held(&self, nick: &str) -> Result<(), Error> {
        let nick = nick.to_owned();
        self.run(move |client| async move {
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
        let nick = nick.to_owned();
        self.run(move |client| async move {
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
