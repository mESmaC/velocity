//! The server's single event log is the feed; sequence numbers are never local.
use crate::{
    Entry, Error,
    remote::{RECORD_PREFIX, Remote},
};
use rosen_client::protocol::channel::{ChannelEvent, ChannelMessage, Member, PieceClaim};
use rosen_client::{
    Client,
    protocol::{Event, EventKind},
};

pub(crate) fn time(milliseconds: i64) -> String {
    chrono::DateTime::from_timestamp_millis(milliseconds)
        .map(|time| time.to_rfc3339())
        .unwrap_or_default()
}

pub(crate) fn message_entry(message: &ChannelMessage) -> Entry {
    let mut record = crate::channel::entry(
        &[],
        "message",
        message.member.as_deref().unwrap_or("you"),
        &message.from,
        &message.text,
    );
    record.sequence = message.seq;
    record.time = time(message.time);
    record.to = message.to.clone();
    record.intent = Some(message.intent.as_str().into());
    record
}

pub(crate) fn claim(claim: &PieceClaim, files: Vec<String>) -> crate::Claim {
    crate::claims::from_server(
        &claim.member,
        &claim.nick,
        &claim.piece,
        &claim.task,
        &claim.branch,
        files,
        time(claim.since),
    )
}

fn member_entry(member: &Member, kind: &str) -> Entry {
    let mut record = crate::channel::entry(&[], kind, &member.member, &member.nick, kind);
    record.time = time(member.left_at.unwrap_or(member.joined_at));
    record.message_mode = Some(match member.mode {
        rosen_client::protocol::channel::MessageMode::Open => crate::MessageMode::Open,
        rosen_client::protocol::channel::MessageMode::HeadsDown => crate::MessageMode::HeadsDown,
        rosen_client::protocol::channel::MessageMode::Quarantined => {
            crate::MessageMode::Quarantined
        }
    });
    record
}

pub(crate) fn channel_entry(event: ChannelEvent) -> Result<Vec<Entry>, Error> {
    let records = match event {
        ChannelEvent::Joined { member } => vec![member_entry(&member, "join")],
        ChannelEvent::Left { member } => vec![member_entry(&member, "leave")],
        ChannelEvent::Mode { member } => vec![member_entry(&member, "message_mode")],
        ChannelEvent::Message { message } => vec![message_entry(&message)],
        ChannelEvent::Claim { claim: held, .. } => vec![claim_entry(&held)],
        ChannelEvent::Refused { nick, holder } => vec![crate::channel::entry(
            &[],
            "refused",
            "velocity",
            &nick,
            &format!(
                "refused {}; held by {} since {}",
                holder.task,
                holder.nick,
                time(holder.since)
            ),
        )],
        ChannelEvent::Released { pieces } => pieces
            .iter()
            .map(|held| {
                let mut record =
                    crate::channel::entry(&[], "released", "velocity", &held.nick, &held.task);
                record.claim = Some(claim(held, Vec::new()));
                record
            })
            .collect(),
        ChannelEvent::Ledger { entry: stored } => vec![ledger_entry(stored)?],
    };
    Ok(records)
}

fn claim_entry(held: &PieceClaim) -> Entry {
    let mut record = crate::channel::entry(
        &[],
        "claim",
        &held.member,
        &held.nick,
        &format!("claimed {}", held.task),
    );
    record.time = time(held.since);
    record.claim = Some(claim(held, Vec::new()));
    record
}

fn ledger_entry(stored: rosen_client::protocol::channel::LedgerEntry) -> Result<Entry, Error> {
    if let Some(text) = stored
        .text
        .strip_prefix(RECORD_PREFIX)
        .filter(|_| stored.member.is_none())
    {
        return Ok(serde_json::from_str(text)?);
    }
    let mut record = crate::channel::entry(
        &[],
        stored.ledger.as_str(),
        stored.member.as_deref().unwrap_or("you"),
        &stored.nick,
        &stored.text,
    );
    record.time = time(stored.time);
    record.velocity_issue = stored.velocity_issue;
    if record.kind == "discovery"
        && record.velocity_issue.is_none()
        && let Some(discovery) = record
            .text
            .strip_prefix(crate::remote::DISCOVERY_PREFIX)
            .and_then(|text| serde_json::from_str::<crate::remote::RosenDiscovery>(text).ok())
    {
        record.text = discovery.text;
        record.rosen_issue = Some(discovery.rosen_issue);
    }
    Ok(record)
}

pub(crate) fn event_entries(event: &Event) -> Result<Vec<Entry>, Error> {
    let mut records = if event.kind == EventKind::Channel {
        channel_entry(serde_json::from_value(event.detail.clone())?)?
    } else {
        project_entry(event).into_iter().collect()
    };
    for record in &mut records {
        record.sequence = event.seq;
        if matches!(record.kind.as_str(), "refused" | "released") {
            record.time.clear();
        }
    }
    Ok(records)
}

fn project_entry(event: &Event) -> Option<Entry> {
    let kind = match event.kind {
        EventKind::Claim => match event.detail.get("kind").and_then(|value| value.as_str()) {
            Some("share") => "shares",
            Some("wait") => "waits",
            _ => "claim",
        },
        EventKind::Released => {
            if event.detail.get("wake").is_some() {
                "unblocked"
            } else {
                "released"
            }
        }
        EventKind::Landed => "merged",
        EventKind::Conflict => "conflict",
        EventKind::Rebased => "rebased",
        EventKind::Reverted => "reverted",
        _ => return None,
    };
    let owner = event
        .detail
        .get("owner")
        .and_then(|value| value.as_str())
        .unwrap_or("velocity");
    let mut record = crate::channel::entry(&[], kind, "velocity", owner, &event.detail.to_string());
    // Project events do not carry wall-clock timestamps. Keep their projection stable.
    record.time.clear();
    record.head = event.change.clone();
    record.files = event
        .detail
        .get("paths")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    Some(record)
}

impl Remote {
    pub(crate) fn read(&self, after: u64) -> Result<Vec<Entry>, Error> {
        let mut records = self.run(async |client| {
            let mut records = read_events(&client, None, 0).await?;
            route_project_entries(&client, &mut records).await?;
            Ok(records)
        })?;
        records.retain(|record| record.sequence > after);
        for record in &mut records {
            self.local_record(record);
        }
        Ok(records)
    }

    pub(crate) fn delivery_cursor(&self, member: &str, saved: u64) -> Result<u64, Error> {
        self.run(async |client| {
            let members = client.channel_presence().await?;
            let Some(_known) = members
                .iter()
                .find(|known| known.member == self.member(member))
            else {
                return Ok(saved);
            };
            if saved != 0 {
                return Ok(saved);
            }
            Ok(read_events(&client, None, 0)
                .await?
                .iter()
                .find(|record| record.member == self.member(member) && record.kind == "join")
                .map_or(0, |record| record.sequence))
        })
    }

    pub(crate) fn acknowledged_cursor(&self, nick: &str) -> Result<u64, Error> {
        self.run(async |client| {
            Ok(client
                .ack_events(rosen_client::identity::parse_agent(nick)?, 0)
                .await?)
        })
    }

    pub(crate) fn acknowledge_cursor(&self, nick: &str, cursor: u64) -> Result<(), Error> {
        self.run(async |client| {
            client
                .ack_events(rosen_client::identity::parse_agent(nick)?, cursor)
                .await?;
            Ok(())
        })
    }

    pub(crate) fn targeted(&self, nick: &str, after: u64, idle: bool) -> Result<Vec<Entry>, Error> {
        self.run(async |client| {
            if idle
                && let Some(member) = client
                    .channel_presence()
                    .await?
                    .iter()
                    .find(|member| member.nick == nick)
            {
                client.channel_release_held(&member.member).await?;
            }
            let mut records = read_events(&client, Some(nick), after).await?;
            for record in &mut records {
                self.local_record(record);
            }
            Ok(records)
        })
    }
}

pub(crate) async fn read_events(
    client: &Client,
    nick: Option<&str>,
    after: u64,
) -> Result<Vec<Entry>, Error> {
    let tip = client.status().await?.last_event;
    let mut events = match nick {
        Some(nick) => {
            client
                .subscribe_as(rosen_client::identity::parse_agent(nick)?, Some(after))
                .await?
        }
        None => client.subscribe(Some(after)).await?,
    };
    let mut records = Vec::new();
    let mut reached = after;
    // A cache daemon reports no watermark, and a targeted stream omits unrelated
    // sequences. A quiet stream ends this page; later events are replayed next read.
    loop {
        let timeout = if nick.is_none() && reached < tip {
            std::time::Duration::from_secs(30)
        } else {
            std::time::Duration::from_millis(100)
        };
        let received = tokio::time::timeout(timeout, events.recv()).await;
        let event = match received {
            Ok(Some(event)) => event,
            Ok(None) => return Err(rosen_client::Error::Disconnected.into()),
            Err(_) if nick.is_none() && reached < tip => {
                return Err(Error::Invalid(
                    "Rosen event replay timed out before its watermark".into(),
                ));
            }
            Err(_) => break,
        };
        reached = event.seq;
        let mut projected = event_entries(&event)?;
        if let Some(nick) = nick {
            target_project_event(&event, nick, &mut projected);
            if event.kind == EventKind::Claim
                && projected.iter().any(|record| record.kind == "overlap")
                && let Ok(Some(detail)) = tokio::time::timeout(
                    std::time::Duration::from_millis(750),
                    overlap_symbols(client, &event),
                )
                .await
            {
                let text = detail.to_string();
                for record in &mut projected {
                    record.text.clone_from(&text);
                }
            }
        }
        records.extend(projected);
        if tip != 0 && event.seq >= tip {
            break;
        }
    }
    Ok(records)
}

async fn overlap_symbols(client: &Client, event: &Event) -> Option<serde_json::Value> {
    let mut detail = event.detail.clone();
    let requester = event.change.as_deref()?;
    let overlaps = detail.get_mut("overlaps")?.as_array_mut()?;
    for overlap in overlaps.iter_mut().take(8) {
        let parsed: rosen_client::protocol::OverlapRecord =
            serde_json::from_value(overlap.clone()).ok()?;
        let symbols =
            velocity_graph::coordination::rosen_overlap_symbols(client, requester, &parsed).await;
        if symbols
            .as_object()
            .is_some_and(|symbols| !symbols.is_empty())
        {
            overlap["symbols"] = symbols;
        }
    }
    Some(detail)
}

fn target_project_event(event: &Event, nick: &str, records: &mut [Entry]) {
    let kind = match event.kind {
        EventKind::Claim
            if event
                .detail
                .get("overlaps")
                .and_then(|value| value.as_array())
                .is_some_and(|overlaps| !overlaps.is_empty()) =>
        {
            "overlap"
        }
        EventKind::Conflict => "conflict",
        EventKind::Rebased => "updated",
        EventKind::Released
            if event.detail.get("wake").and_then(|value| value.as_bool()) == Some(true) =>
        {
            "unblocked"
        }
        EventKind::Reverted => "reverted",
        _ => return,
    };
    for record in records {
        record.kind = kind.into();
        record.to.push(nick.into());
    }
}

// Rosen's change owner is the full workspace name; the channel nick can be
// shortened or suffixed. Join them by the server's registered change/member IDs.
async fn route_project_entries(client: &Client, records: &mut [Entry]) -> Result<(), Error> {
    let members = client.channel_presence().await?;
    let changes = client
        .change_list(crate::remote::wire(serde_json::json!({}))?)
        .await?;
    let mut nicks = std::collections::BTreeMap::new();
    for record in records.iter().filter(|record| record.checkout.is_some()) {
        if let Some(change) = &record.head
            && let Some(member) = members.iter().find(|member| member.member == record.member)
        {
            nicks.insert(change.clone(), member.nick.clone());
        }
    }
    let owners: std::collections::BTreeMap<_, _> = changes
        .iter()
        .filter_map(|change| {
            nicks
                .get(&change.change.id.to_string())
                .map(|nick| (change.change.owner.to_string(), nick.clone()))
        })
        .collect();
    for record in records {
        let Some(nick) = record.head.as_ref().and_then(|change| nicks.get(change)) else {
            continue;
        };
        record.nick = nick.clone();
        match record.kind.as_str() {
            "conflict" | "rebased" => record.to.push(nick.clone()),
            "claim" if record.claim.is_none() => route_overlap(record, nick, &owners),
            _ => {}
        }
    }
    Ok(())
}

fn route_overlap(
    record: &mut Entry,
    nick: &str,
    owners: &std::collections::BTreeMap<String, String>,
) {
    let Ok(detail) = serde_json::from_str::<serde_json::Value>(&record.text) else {
        return;
    };
    let Some(overlaps) = detail
        .get("overlaps")
        .and_then(|value| value.as_array())
        .filter(|rows| !rows.is_empty())
    else {
        return;
    };
    record.kind = "overlap".into();
    record.to.push(nick.into());
    for overlap in overlaps {
        if let Some(owner) = overlap.get("owner").and_then(|value| value.as_str()) {
            record
                .to
                .push(owners.get(owner).cloned().unwrap_or_else(|| owner.into()));
        }
    }
}
