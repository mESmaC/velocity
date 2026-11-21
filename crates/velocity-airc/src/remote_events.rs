//! The server's single event log is the feed; sequence numbers are never local.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::UNIX_EPOCH;

use crate::{
    Entry, Error, EventPayload, OverlapDetail, OverlapSymbols,
    channel::{format_timestamp_millis, parse_timestamp_millis},
    payload::overlap_text,
    remote::{RECORD_PREFIX, Remote},
};
use rosen_client::protocol::channel::{ChannelEvent, ChannelMessage, Member, PieceClaim};
use rosen_client::{
    Client,
    protocol::{Event, EventKind},
};

#[derive(Default)]
struct RemoteEventCache {
    identity: Option<String>,
    tip: u64,
    synced_once: bool,
    entries: Vec<Entry>,
    /// Scoped member -> latest registered checkout change ID.
    checkouts: BTreeMap<String, String>,
    /// Change ID -> scoped member from checkout records.
    change_members: BTreeMap<String, String>,
}

impl RemoteEventCache {
    fn reset(&mut self) {
        self.tip = 0;
        self.synced_once = false;
        self.entries.clear();
        self.checkouts.clear();
        self.change_members.clear();
    }

    fn push_all(&mut self, delta: Vec<Entry>) {
        for record in delta {
            if record.sequence > self.tip {
                self.tip = record.sequence;
            }
            if record.checkout.is_some()
                && let Some(change) = record.change_id()
            {
                self.checkouts
                    .insert(record.member.clone(), change.to_owned());
                self.change_members
                    .insert(change.to_owned(), record.member.clone());
            }
            self.entries.push(record);
        }
    }
}

type EndpointCaches = Mutex<HashMap<String, Arc<Mutex<RemoteEventCache>>>>;
static REMOTE_CACHES: OnceLock<EndpointCaches> = OnceLock::new();

fn endpoint_cache(endpoint: &str) -> Arc<Mutex<RemoteEventCache>> {
    let mut map = REMOTE_CACHES
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    Arc::clone(map.entry(endpoint.to_owned()).or_default())
}

fn database_identity(data_dir: &str) -> String {
    let database = Path::new(data_dir).join("metadata.sqlite");
    let created = std::fs::metadata(&database)
        .and_then(|metadata| metadata.created())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    match created {
        Some(nanos) => format!("{data_dir}:{nanos}"),
        None => data_dir.to_owned(),
    }
}

pub(crate) fn time(milliseconds: i64) -> String {
    format_timestamp_millis(milliseconds).unwrap_or_default()
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
    // Mode changes carry no timestamp of their own; `apply_timestamps` assigns the
    // server event time so a later mode change never borrows an earlier `joined_at`.
    if kind == "message_mode" {
        record.time.clear();
    } else {
        record.time = time(member.left_at.unwrap_or(member.joined_at));
    }
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
        ChannelEvent::Claim {
            claim: held,
            extended,
        } => vec![claim_entry(&held, extended)],
        ChannelEvent::Refused { nick, holder } => {
            let mut record = crate::channel::entry(
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
            );
            record.time.clear();
            vec![record]
        }
        ChannelEvent::Released { pieces } => pieces
            .iter()
            .map(|held| {
                let mut record =
                    crate::channel::entry(&[], "released", "velocity", &held.nick, &held.task);
                record.time.clear();
                record.claim = Some(claim(held, Vec::new()));
                record
            })
            .collect(),
        ChannelEvent::Ledger { entry: stored } => vec![ledger_entry(stored)?],
    };
    Ok(records)
}

fn claim_entry(held: &PieceClaim, extended: bool) -> Entry {
    let mut record = crate::channel::entry(
        &[],
        "claim",
        &held.member,
        &held.nick,
        &format!("claimed {}", held.task),
    );
    // An extended claim keeps the original `since`; leave `time` for the server event time.
    if extended {
        record.time.clear();
    } else {
        record.time = time(held.since);
    }
    record.claim = Some(claim(held, Vec::new()));
    record
}

fn ledger_entry(stored: rosen_client::protocol::channel::LedgerEntry) -> Result<Entry, Error> {
    if let Some(text) = stored
        .text
        .strip_prefix(RECORD_PREFIX)
        .filter(|_| stored.member.is_none())
    {
        let mut record: Entry = serde_json::from_str(text)?;
        // Use the server's ledger timestamp (later refined by `events.created_at`) instead
        // of unnormalized client clocks stored inside the envelope (#540).
        record.time = time(stored.time);
        normalize_envelope(&mut record);
        return Ok(record);
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

fn normalize_envelope(record: &mut Entry) {
    if let Some(claim) = &mut record.claim
        && let Some(millis) = parse_timestamp_millis(&claim.since)
    {
        claim.since = time(millis);
    }
    if record.kind == "checkout" {
        if record.change.is_none() {
            record.change = record.head.take();
        }
        if let Some(checkout) = record.checkout.take() {
            record.checkout = Some(checkout.into_rosen(record.change.clone()));
        }
        if record.text == "thread checkout" {
            record.text = "thread workspace".into();
        }
    }
}

pub(crate) fn event_entries(event: &Event) -> Result<Vec<Entry>, Error> {
    let mut records = if event.kind == EventKind::Channel {
        channel_entry(serde_json::from_value(event.detail.clone())?)?
    } else {
        project_entry(event).into_iter().collect()
    };
    for record in &mut records {
        record.sequence = event.seq;
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
    let (text, payload) = EventPayload::from_rosen(event, kind, owner)?;
    let mut record = crate::channel::entry(&[], kind, "velocity", owner, &text);
    // Filled from Rosen's `events.created_at` (or monotonic neighbor) in `apply_timestamps`.
    record.time.clear();
    record.change = event.change.clone();
    record.payload = Some(payload);
    record.files = event
        .detail
        .get("paths")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    Some(record)
}

/// Assigns uniform, monotonic UTC millisecond timestamps to every projected record,
/// preferring the server's authoritative `events.created_at` column (#540).
fn apply_timestamps(records: &mut [Entry], floor: Option<i64>, server_times: &BTreeMap<u64, i64>) {
    let candidates: Vec<Option<i64>> = records
        .iter()
        .map(|record| {
            server_times
                .get(&record.sequence)
                .copied()
                .or_else(|| parse_timestamp_millis(&record.time))
        })
        .collect();
    let fallback = floor
        .or_else(|| candidates.iter().flatten().copied().next())
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let mut current = floor;
    for (record, candidate) in records.iter_mut().zip(candidates) {
        let resolved = match (candidate, current) {
            (Some(stamp), Some(previous)) => stamp.max(previous),
            (Some(stamp), None) => stamp,
            (None, Some(previous)) => previous,
            (None, None) => fallback,
        };
        current = Some(resolved);
        record.time = time(resolved);
    }
}

impl Remote {
    /// Syncs the per-endpoint incremental event cache up to Rosen's current `last_event` (#541).
    fn sync_cache(&self) -> Result<Arc<Mutex<RemoteEventCache>>, Error> {
        let cache_arc = endpoint_cache(self.endpoint.as_str());
        let (data_dir, identity, tip) = self.run(async |client| {
            let status = client.status().await?;
            let identity = database_identity(&status.data_dir);
            Ok((status.data_dir, identity, status.last_event))
        })?;
        let (start_after, reset) = {
            let cache = cache_arc.lock().unwrap_or_else(|error| error.into_inner());
            let changed = cache.identity.as_deref() != Some(identity.as_str()) || tip < cache.tip;
            if !changed && cache.synced_once && tip != 0 && tip == cache.tip {
                drop(cache);
                return Ok(cache_arc);
            }
            if changed {
                (0, true)
            } else {
                (cache.tip, false)
            }
        };
        let delta = if tip != 0 && start_after >= tip {
            Vec::new()
        } else {
            self.run_dedicated(async |client| {
                read_events_with_status(client, None, start_after, tip, &data_dir).await
            })?
        };
        let mut cache = cache_arc.lock().unwrap_or_else(|error| error.into_inner());
        if reset || cache.identity.as_deref() != Some(identity.as_str()) || tip < cache.tip {
            cache.reset();
        }
        cache.identity = Some(identity);
        // Guard against a concurrent caller advancing past start_after before we re-locked.
        let filtered: Vec<Entry> = delta
            .into_iter()
            .filter(|record| record.sequence > cache.tip)
            .collect();
        cache.push_all(filtered);
        cache.tip = cache.tip.max(tip);
        cache.synced_once = true;
        drop(cache);
        Ok(cache_arc)
    }

    pub(crate) fn warm(&self) -> Result<(), Error> {
        self.sync_cache().map(drop)
    }

    pub(crate) fn checkouts(&self) -> Result<BTreeMap<String, String>, Error> {
        let cache_arc = self.sync_cache()?;
        let cache = cache_arc.lock().unwrap_or_else(|error| error.into_inner());
        Ok(cache.checkouts.clone())
    }

    pub(crate) fn read(&self, after: u64) -> Result<Vec<Entry>, Error> {
        let cache_arc = self.sync_cache()?;
        let (mut records, change_members) = {
            let cache = cache_arc.lock().unwrap_or_else(|error| error.into_inner());
            let start = cache
                .entries
                .partition_point(|record| record.sequence <= after);
            let slice = cache.entries[start..].to_vec();
            let needs_routing = slice.iter().any(|record| record.change_id().is_some());
            let members = if needs_routing {
                Some(cache.change_members.clone())
            } else {
                None
            };
            (slice, members)
        };
        if let Some(change_members) = change_members {
            self.run(async |client| {
                route_project_entries(client, &change_members, &mut records).await
            })?;
        }
        for record in &mut records {
            self.local_record(record);
        }
        Ok(records)
    }

    pub(crate) fn read_ledger(&self, ledger: &str, after: u64) -> Result<Vec<Entry>, Error> {
        let cache_arc = self.sync_cache()?;
        let mut records: Vec<Entry> = {
            let cache = cache_arc.lock().unwrap_or_else(|error| error.into_inner());
            let start = cache
                .entries
                .partition_point(|record| record.sequence <= after);
            cache.entries[start..]
                .iter()
                .filter(|record| record.kind == ledger)
                .cloned()
                .collect()
        };
        for record in &mut records {
            self.local_record(record);
        }
        Ok(records)
    }

    pub(crate) fn latest_task_list(&self) -> Result<Option<String>, Error> {
        let cache_arc = self.sync_cache()?;
        let cache = cache_arc.lock().unwrap_or_else(|error| error.into_inner());
        Ok(cache
            .entries
            .iter()
            .rev()
            .find(|record| record.kind == "task_list")
            .map(|record| record.text.clone()))
    }

    pub(crate) fn delivery_cursor(&self, member: &str, saved: u64) -> Result<u64, Error> {
        let scoped = self.member(member);
        let present = self.run(async |client| {
            Ok(client
                .channel_presence()
                .await?
                .iter()
                .any(|known| known.member == scoped))
        })?;
        if !present || saved != 0 {
            return Ok(saved);
        }
        let cache_arc = self.sync_cache()?;
        let cache = cache_arc.lock().unwrap_or_else(|error| error.into_inner());
        Ok(cache
            .entries
            .iter()
            .find(|record| record.member == scoped && record.kind == "join")
            .map_or(0, |record| record.sequence))
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
        if idle {
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
            })?;
        }
        let (data_dir, tip) = self.run(async |client| {
            let status = client.status().await?;
            Ok((status.data_dir, status.last_event))
        })?;
        if tip != 0 && after >= tip {
            return Ok(Vec::new());
        }
        self.run_dedicated(async |client| {
            let mut records =
                read_events_with_status(client, Some(nick), after, tip, &data_dir).await?;
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
    let status = client.status().await?;
    read_events_with_status(client, nick, after, status.last_event, &status.data_dir).await
}

async fn read_events_with_status(
    client: &Client,
    nick: Option<&str>,
    after: u64,
    tip: u64,
    data_dir: &str,
) -> Result<Vec<Entry>, Error> {
    if tip != 0 && after >= tip {
        return Ok(Vec::new());
    }
    let mut events = subscribe_events(client, nick, after).await?;
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
        let event = match tokio::time::timeout(timeout, events.recv()).await {
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
        let projected = project_stream_event(client, &event, nick).await?;
        records.extend(projected);
        if tip != 0 && event.seq >= tip {
            break;
        }
    }
    let data_dir = PathBuf::from(data_dir);
    let (floor, server_times) =
        tokio::task::spawn_blocking(move || velocity_rosen::event_timestamps(&data_dir, after))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
    apply_timestamps(&mut records, floor, &server_times);
    Ok(records)
}

async fn subscribe_events(
    client: &Client,
    nick: Option<&str>,
    after: u64,
) -> Result<tokio::sync::mpsc::Receiver<Event>, Error> {
    match nick {
        Some(nick) => Ok(client
            .subscribe_as(rosen_client::identity::parse_agent(nick)?, Some(after))
            .await?),
        None => Ok(client.subscribe(Some(after)).await?),
    }
}

async fn project_stream_event(
    client: &Client,
    event: &Event,
    nick: Option<&str>,
) -> Result<Vec<Entry>, Error> {
    let mut projected = event_entries(event)?;
    if let Some(nick) = nick {
        target_project_event(event, nick, &mut projected);
        if event.kind == EventKind::Claim
            && projected.iter().any(|record| record.kind == "overlap")
            && let Ok(Some(overlaps)) = tokio::time::timeout(
                std::time::Duration::from_millis(750),
                overlap_symbols(client, event),
            )
            .await
        {
            apply_overlap_symbols(&mut projected, &overlaps);
        }
    }
    Ok(projected)
}

async fn overlap_symbols(
    client: &Client,
    event: &Event,
) -> Option<Vec<BTreeMap<String, OverlapSymbols>>> {
    let requester = event.change.as_deref()?;
    let overlaps = event.detail.get("overlaps")?.as_array()?;
    let mut resolved = Vec::new();
    for overlap in overlaps.iter().take(8) {
        let parsed: rosen_client::protocol::OverlapRecord =
            serde_json::from_value(overlap.clone()).ok()?;
        let value =
            velocity_graph::coordination::rosen_overlap_symbols(client, requester, &parsed).await;
        let symbols =
            serde_json::from_value::<BTreeMap<String, OverlapSymbols>>(value).unwrap_or_default();
        resolved.push(symbols);
    }
    Some(resolved)
}

fn apply_overlap_symbols(records: &mut [Entry], resolved: &[BTreeMap<String, OverlapSymbols>]) {
    for record in records {
        if let Some(EventPayload::PathClaim { overlaps, .. }) = &mut record.payload {
            for (overlap, symbols) in overlaps.iter_mut().zip(resolved) {
                if !symbols.is_empty() {
                    overlap.symbols.clone_from(symbols);
                }
            }
            if !overlaps.is_empty() {
                record.text = overlap_text(&record.nick, overlaps);
            }
        }
    }
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
async fn route_project_entries(
    client: &Client,
    change_members: &BTreeMap<String, String>,
    records: &mut [Entry],
) -> Result<(), Error> {
    let (members, changes) = tokio::try_join!(
        client.channel_presence(),
        client.change_list(crate::remote::wire(serde_json::json!({}))?),
    )?;
    let mut nicks = BTreeMap::new();
    for (change, member_id) in change_members {
        if let Some(member) = members.iter().find(|member| &member.member == member_id) {
            nicks.insert(change.clone(), member.nick.clone());
        }
    }
    for change in &changes {
        nicks
            .entry(change.change.id.to_string())
            .or_insert_with(|| change.change.owner.to_string());
    }
    let owners: BTreeMap<_, _> = changes
        .iter()
        .filter_map(|change| {
            nicks
                .get(&change.change.id.to_string())
                .map(|nick| (change.change.owner.to_string(), nick.clone()))
        })
        .collect();
    for record in records {
        let Some(nick) = record.change_id().and_then(|change| nicks.get(change)) else {
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

fn route_overlap(record: &mut Entry, nick: &str, owners: &BTreeMap<String, String>) {
    let Some(EventPayload::PathClaim { overlaps, .. }) = &mut record.payload else {
        return;
    };
    if overlaps.is_empty() {
        return;
    }
    record.kind = "overlap".into();
    record.to.push(nick.into());
    for overlap in overlaps.iter_mut() {
        let holder = owners
            .get(&overlap.owner)
            .cloned()
            .unwrap_or_else(|| overlap.owner.clone());
        overlap.owner = holder.clone();
        record.to.push(holder);
    }
    record.text = overlap_text(nick, overlaps);
}

/// Keep the recipients resolved by the project view and the symbol details
/// resolved by the targeted view of the same event.
pub(crate) fn copy_overlap_symbols(project: &mut Entry, targeted: &Entry) {
    let (
        Some(EventPayload::PathClaim {
            overlaps: project_rows,
            ..
        }),
        Some(EventPayload::PathClaim {
            overlaps: targeted_rows,
            ..
        }),
    ) = (&mut project.payload, &targeted.payload)
    else {
        return;
    };
    copy_rows(project_rows, targeted_rows);
    if !project_rows.is_empty() {
        project.text = overlap_text(&project.nick, project_rows);
    }
}

fn copy_rows(project_rows: &mut [OverlapDetail], targeted_rows: &[OverlapDetail]) {
    for (project_row, targeted_row) in project_rows.iter_mut().zip(targeted_rows) {
        if !targeted_row.symbols.is_empty() {
            project_row.symbols.clone_from(&targeted_row.symbols);
        }
    }
}
