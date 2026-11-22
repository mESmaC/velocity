//! The server's single event log is the feed; sequence numbers are never local.
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, UNIX_EPOCH};

use crate::{
    Entry, Error, EventPayload, OverlapDetail, OverlapSymbols,
    channel::{format_timestamp_millis, parse_timestamp_millis},
    payload::overlap_text,
    remote::{
        RECORD_PREFIX, Remote, is_transport_error, record_connect_duration, run_on_shared_runtime,
    },
};
use rosen_client::protocol::channel::{ChannelEvent, ChannelMessage, Member, PieceClaim};
use rosen_client::{
    Client, Endpoint,
    protocol::{Event, EventKind},
};

struct RoutedSnapshot {
    scope: String,
    tip: u64,
    entries: Arc<Vec<Entry>>,
}

struct TargetedCheck {
    identity: String,
    tip: u64,
    after: u64,
    idle: bool,
}

#[derive(Default)]
struct RemoteEventCache {
    identity: Option<String>,
    data_dir: Option<String>,
    tip: u64,
    synced_once: bool,
    entries: Vec<Entry>,
    /// Scoped member -> latest registered checkout change ID.
    checkouts: BTreeMap<String, String>,
    /// Change ID -> scoped member from checkout records.
    change_members: BTreeMap<String, String>,
    /// Scoped member -> latest AIRC nick.
    member_nicks: BTreeMap<String, String>,
    /// AIRC nick -> latest registered checkout change ID.
    nick_changes: BTreeMap<String, String>,
    /// Server ledger sequence -> server event sequence for `append_record` (#555).
    ledger_event_seq: BTreeMap<u64, u64>,
    /// Cached routed & localized snapshot for the current `(scope, tip)` (#555).
    routed: Option<RoutedSnapshot>,
    /// Last targeted-stream check per nick so quiet ticks skip `subscribe_as` (#555).
    targeted_checks: HashMap<String, TargetedCheck>,
}

impl RemoteEventCache {
    fn reset(&mut self) {
        self.tip = 0;
        self.synced_once = false;
        self.entries.clear();
        self.checkouts.clear();
        self.change_members.clear();
        self.member_nicks.clear();
        self.nick_changes.clear();
        self.ledger_event_seq.clear();
        self.routed = None;
        self.targeted_checks.clear();
    }

    fn push_all(&mut self, delta: Vec<Entry>, ledger_seqs: Vec<(u64, u64)>) {
        let mut changed = false;
        for (ledger_seq, event_seq) in ledger_seqs {
            self.ledger_event_seq.insert(ledger_seq, event_seq);
        }
        for mut record in delta {
            changed = true;
            if record.sequence > self.tip {
                self.tip = record.sequence;
            }
            if matches!(record.kind.as_str(), "join" | "leave")
                && !record.member.is_empty()
                && !record.nick.is_empty()
            {
                self.member_nicks
                    .insert(record.member.clone(), record.nick.clone());
                if let Some(change) = self.checkouts.get(&record.member) {
                    self.nick_changes
                        .insert(record.nick.clone(), change.clone());
                }
            }
            if record.checkout.is_some()
                && let Some(change) = record.change_id().map(str::to_owned)
            {
                self.checkouts.insert(record.member.clone(), change.clone());
                self.change_members
                    .insert(change.clone(), record.member.clone());
                if let Some(nick) = self.member_nicks.get(&record.member) {
                    self.nick_changes.insert(nick.clone(), change.clone());
                }
                if !matches!(record.nick.as_str(), "" | "velocity" | "you") {
                    self.nick_changes.insert(record.nick.clone(), change);
                }
            }
            self.attach_change(&mut record);
            self.entries.push(record);
        }
        if changed {
            self.routed = None;
        }
    }

    /// Attaches the active Rosen change ID to change-related AIRC entries that did not
    /// carry one directly from Rosen (#532).
    fn attach_change(&self, record: &mut Entry) {
        if record.change.is_none() && is_change_related(&record.kind) {
            let resolved = record
                .claim
                .as_ref()
                .and_then(|claim| self.checkouts.get(&claim.member))
                .or_else(|| {
                    record
                        .dependency
                        .as_ref()
                        .and_then(|wait| self.checkouts.get(&wait.member))
                })
                .or_else(|| {
                    (!matches!(record.member.as_str(), "" | "velocity" | "you"))
                        .then(|| self.checkouts.get(&record.member))
                        .flatten()
                })
                .or_else(|| {
                    (!matches!(record.nick.as_str(), "" | "velocity" | "you"))
                        .then(|| self.nick_changes.get(&record.nick))
                        .flatten()
                });
            record.change = resolved.cloned();
        }
        if let Some(claim) = &mut record.claim
            && claim.change.is_none()
        {
            claim.change = record.change.clone();
        }
    }
}

fn is_change_related(kind: &str) -> bool {
    matches!(
        kind,
        "claim"
            | "refused"
            | "overlap"
            | "shares"
            | "waits"
            | "unblocked"
            | "PR"
            | "changes"
            | "pushed"
            | "approved"
            | "merged"
            | "failed"
            | "left"
            | "rebased"
            | "updated"
            | "conflict"
            | "stalled"
            | "at limit"
            | "released"
            | "broken"
            | "reverted"
            | "past_work"
            | "discovery"
            | "join"
            | "leave"
    )
}

struct PersistentProjectStream {
    _client: Client,
    events: tokio::sync::mpsc::Receiver<Event>,
    identity: String,
}

#[derive(Default)]
struct EndpointState {
    cache: Mutex<RemoteEventCache>,
    /// One long-lived project-wide event subscription per endpoint on the shared runtime (#555).
    project_stream: tokio::sync::Mutex<Option<PersistentProjectStream>>,
    /// One long-lived targeted subscription client per endpoint on the shared runtime (#555).
    targeted_client: tokio::sync::Mutex<Option<Client>>,
}

type EndpointStates = Mutex<HashMap<String, Arc<EndpointState>>>;
static REMOTE_STATES: OnceLock<EndpointStates> = OnceLock::new();

fn endpoint_state(endpoint: &str) -> Arc<EndpointState> {
    let mut map = REMOTE_STATES
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

pub(crate) fn claim(
    claim: &PieceClaim,
    files: Vec<String>,
    change: Option<String>,
) -> crate::Claim {
    crate::claims::from_server(
        &claim.member,
        &claim.nick,
        &claim.piece,
        &claim.task,
        &claim.branch,
        change,
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
                record.claim = Some(claim(held, Vec::new(), None));
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
    record.claim = Some(claim(held, Vec::new(), None));
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
    event_entries_with_ledger(event).map(|(records, _)| records)
}

fn event_entries_with_ledger(event: &Event) -> Result<(Vec<Entry>, Option<u64>), Error> {
    let (mut records, ledger_seq) = if event.kind == EventKind::Channel {
        let channel_event: ChannelEvent = serde_json::from_value(event.detail.clone())?;
        let ledger_seq = match &channel_event {
            ChannelEvent::Ledger { entry } => Some(entry.seq),
            _ => None,
        };
        (channel_entry(channel_event)?, ledger_seq)
    } else {
        (project_entry(event).into_iter().collect(), None)
    };
    for record in &mut records {
        record.sequence = event.seq;
    }
    Ok((records, ledger_seq))
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
        EventKind::DeployDue => "deploy",
        EventKind::DeployResult => {
            match event.detail.get("status").and_then(|value| value.as_str()) {
                Some("passed") => "deployed",
                Some("failed") => "deploy failed",
                Some("skipped") => "deploy skipped",
                _ => "deploy",
            }
        }
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
    /// Syncs the per-endpoint incremental event cache up to Rosen's current `last_event`
    /// using one persistent project stream per endpoint on the shared runtime (#541, #555).
    fn sync_cache(&self) -> Result<Arc<EndpointState>, Error> {
        let state = endpoint_state(self.endpoint.as_str());
        let (data_dir, identity, tip) = self.run(|client| async move {
            let status = client.status().await?;
            let identity = database_identity(&status.data_dir);
            Ok((status.data_dir, identity, status.last_event))
        })?;
        {
            let cache = state
                .cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if cache.identity.as_deref() == Some(identity.as_str())
                && cache.synced_once
                && tip != 0
                && cache.tip >= tip
            {
                drop(cache);
                return Ok(state);
            }
        }
        let endpoint = self.endpoint.clone();
        let state_for_task = Arc::clone(&state);
        let connect_elapsed = run_on_shared_runtime(async move {
            sync_project_stream(&endpoint, &state_for_task, &data_dir, &identity, tip).await
        })?;
        record_connect_duration(connect_elapsed);
        Ok(state)
    }

    /// Waits on the persistent project stream for the server event corresponding to `ledger_seq`
    /// without opening a new connection or spawning a thread (#555).
    pub(crate) fn wait_ledger_sequence(&self, ledger_seq: u64) -> Result<u64, Error> {
        let state = endpoint_state(self.endpoint.as_str());
        if let Some(seq) = state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .ledger_event_seq
            .get(&ledger_seq)
            .copied()
        {
            return Ok(seq);
        }
        let endpoint = self.endpoint.clone();
        let (sequence, connect_elapsed) = run_on_shared_runtime(async move {
            wait_for_ledger_event(&endpoint, &state, ledger_seq).await
        })?;
        record_connect_duration(connect_elapsed);
        Ok(sequence)
    }

    pub(crate) fn warm(&self) -> Result<(), Error> {
        self.sync_cache().map(drop)
    }

    pub(crate) fn checkouts(&self) -> Result<BTreeMap<String, String>, Error> {
        let state = self.sync_cache()?;
        let cache = state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Ok(cache.checkouts.clone())
    }

    /// Returns a cached routed and localized snapshot of the channel for the current tip (#555).
    pub(crate) fn snapshot(&self) -> Result<Arc<Vec<Entry>>, Error> {
        let state = self.sync_cache()?;
        let (mut records, change_members, snapshot_tip) = {
            let cache = state
                .cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Some(routed) = &cache.routed
                && routed.scope == self.scope
                && routed.tip == cache.tip
                && (cache.tip != 0 || cache.entries.is_empty())
            {
                return Ok(Arc::clone(&routed.entries));
            }
            let slice = cache.entries.clone();
            let needs_routing = slice.iter().any(|record| {
                record.change_id().is_some()
                    || (record.change.is_none() && is_change_related(&record.kind))
            });
            let members = needs_routing.then(|| cache.change_members.clone());
            (slice, members, cache.tip)
        };
        if let Some(change_members) = change_members {
            records = self.run(move |client| async move {
                route_project_entries(&client, &change_members, &mut records).await?;
                Ok(records)
            })?;
        }
        for record in &mut records {
            self.local_record(record);
        }
        let snapshot = Arc::new(records);
        let mut cache = state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if cache.tip == snapshot_tip {
            cache.routed = Some(RoutedSnapshot {
                scope: self.scope.clone(),
                tip: snapshot_tip,
                entries: Arc::clone(&snapshot),
            });
        }
        Ok(snapshot)
    }

    pub(crate) fn read(&self, after: u64) -> Result<Vec<Entry>, Error> {
        let snapshot = self.snapshot()?;
        let start = snapshot.partition_point(|record| record.sequence <= after);
        Ok(snapshot[start..].to_vec())
    }

    pub(crate) fn read_ledger(&self, ledger: &str, after: u64) -> Result<Vec<Entry>, Error> {
        let state = self.sync_cache()?;
        let mut records: Vec<Entry> = {
            let cache = state
                .cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
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
        let state = self.sync_cache()?;
        let cache = state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Ok(cache
            .entries
            .iter()
            .rev()
            .find(|record| record.kind == "task_list")
            .map(|record| record.text.clone()))
    }

    pub(crate) fn delivery_cursor(&self, member: &str, saved: u64) -> Result<u64, Error> {
        let scoped = self.member(member);
        let scoped_for_rpc = scoped.clone();
        let present = self.run(move |client| async move {
            Ok(client
                .channel_presence()
                .await?
                .iter()
                .any(|known| known.member == scoped_for_rpc))
        })?;
        if !present || saved != 0 {
            return Ok(saved);
        }
        let state = self.sync_cache()?;
        let cache = state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Ok(cache
            .entries
            .iter()
            .find(|record| record.member == scoped && record.kind == "join")
            .map_or(0, |record| record.sequence))
    }

    pub(crate) fn acknowledged_cursor(&self, nick: &str) -> Result<u64, Error> {
        let nick = nick.to_owned();
        self.run(move |client| async move {
            Ok(client
                .ack_events(rosen_client::identity::parse_agent(&nick)?, 0)
                .await?)
        })
    }

    pub(crate) fn acknowledge_cursor(&self, nick: &str, cursor: u64) -> Result<(), Error> {
        let nick = nick.to_owned();
        self.run(move |client| async move {
            client
                .ack_events(rosen_client::identity::parse_agent(&nick)?, cursor)
                .await?;
            Ok(())
        })
    }

    pub(crate) fn targeted(&self, nick: &str, after: u64, idle: bool) -> Result<Vec<Entry>, Error> {
        let state = self.sync_cache()?;
        let (identity, data_dir, tip, needs_release, can_skip) = {
            let mut cache = state
                .cache
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let identity = cache.identity.clone().unwrap_or_default();
            let data_dir = cache.data_dir.clone().unwrap_or_default();
            let tip = cache.tip;
            let (needs_release, can_skip) =
                evaluate_targeted_skip(&mut cache, nick, &identity, tip, after, idle);
            (identity, data_dir, tip, needs_release, can_skip)
        };
        if needs_release {
            self.release_held(nick)?;
        }
        if can_skip {
            return Ok(Vec::new());
        }
        let endpoint = self.endpoint.clone();
        let nick_owned = nick.to_owned();
        let state_for_task = Arc::clone(&state);
        let (mut records, connect_elapsed) = run_on_shared_runtime(async move {
            read_targeted_persistent(
                &endpoint,
                &state_for_task,
                &nick_owned,
                after,
                tip,
                &data_dir,
            )
            .await
        })?;
        record_connect_duration(connect_elapsed);
        for record in &mut records {
            self.local_record(record);
        }
        let mut cache = state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if cache.identity.as_deref() == Some(identity.as_str()) {
            cache.targeted_checks.insert(
                nick.to_owned(),
                TargetedCheck {
                    identity,
                    tip,
                    after,
                    idle,
                },
            );
        }
        Ok(records)
    }
}

fn evaluate_targeted_skip(
    cache: &mut RemoteEventCache,
    nick: &str,
    identity: &str,
    tip: u64,
    after: u64,
    idle: bool,
) -> (bool, bool) {
    let previous = cache.targeted_checks.get(nick);
    let needs_release = idle
        && previous.is_none_or(|prev| prev.identity != identity || !prev.idle || prev.tip < tip);
    if tip != 0 && after >= tip {
        return (needs_release, true);
    }
    let Some(prev) = previous else {
        return (needs_release, false);
    };
    if prev.identity != identity || prev.after != after || (idle && !prev.idle) {
        return (needs_release, false);
    }
    if prev.tip >= tip {
        return (needs_release, true);
    }
    let prev_tip = prev.tip;
    if !has_possible_targeted_entry(&cache.entries, prev_tip, tip, nick) {
        if let Some(entry) = cache.targeted_checks.get_mut(nick) {
            entry.tip = tip;
            entry.idle |= idle;
        }
        return (needs_release, true);
    }
    (needs_release, false)
}

fn has_possible_targeted_entry(entries: &[Entry], prev_tip: u64, tip: u64, nick: &str) -> bool {
    let start = entries.partition_point(|entry| entry.sequence <= prev_tip);
    entries[start..]
        .iter()
        .take_while(|entry| entry.sequence <= tip)
        .any(|entry| entry_may_target_nick(entry, nick))
}

fn entry_may_target_nick(entry: &Entry, nick: &str) -> bool {
    match entry.kind.as_str() {
        "message" => {
            entry.nick != nick
                && (entry.to.iter().any(|recipient| recipient == nick)
                    || (entry.nick == "you" && entry.to.is_empty()))
        }
        "message_mode" | "PR" | "pushed" | "updated" => entry.nick == nick,
        "overlap" | "conflict" | "rebased" | "unblocked" | "reverted" => true,
        // Rosen path claim events carry no AIRC piece claim and can produce targeted overlaps.
        "claim" => entry.claim.is_none(),
        _ => false,
    }
}

async fn sync_project_stream(
    endpoint: &Endpoint,
    state: &EndpointState,
    data_dir: &str,
    identity: &str,
    tip: u64,
) -> Result<Duration, Error> {
    let mut stream_guard = state.project_stream.lock().await;
    let (start_after, reset) = {
        let mut cache = state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let changed = cache.identity.as_deref() != Some(identity);
        if !changed && cache.synced_once && tip != 0 && cache.tip >= tip {
            return Ok(Duration::ZERO);
        }
        if changed {
            cache.reset();
            cache.identity = Some(identity.to_owned());
            cache.data_dir = Some(data_dir.to_owned());
            (0, true)
        } else {
            cache.data_dir = Some(data_dir.to_owned());
            (cache.tip, false)
        }
    };
    if reset {
        *stream_guard = None;
    }
    if tip != 0 && start_after >= tip {
        let mut cache = state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        cache.tip = cache.tip.max(tip);
        cache.synced_once = true;
        return Ok(Duration::ZERO);
    }
    let mut total_connect = Duration::ZERO;
    for attempt in 0..2 {
        let current_after = {
            state
                .cache
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .tip
        };
        let (elapsed, stream) =
            ensure_project_stream(endpoint, &mut stream_guard, identity, current_after).await?;
        total_connect = total_connect.saturating_add(elapsed);
        match drain_project_stream(stream, current_after, tip, data_dir).await {
            Ok((records, ledger_seqs, reached)) => {
                commit_project_delta(state, records, ledger_seqs, reached.max(tip));
                return Ok(total_connect);
            }
            Err(error) if attempt == 0 && is_transport_error(&error) => {
                *stream_guard = None;
            }
            Err(error) => {
                if is_transport_error(&error) {
                    *stream_guard = None;
                }
                return Err(error);
            }
        }
    }
    Err(rosen_client::Error::Disconnected.into())
}

async fn ensure_project_stream<'a>(
    endpoint: &Endpoint,
    slot: &'a mut Option<PersistentProjectStream>,
    identity: &str,
    after: u64,
) -> Result<(Duration, &'a mut PersistentProjectStream), Error> {
    let needs_connect = slot
        .as_ref()
        .is_none_or(|stream| stream.identity != identity);
    let mut elapsed = Duration::ZERO;
    if needs_connect {
        let started = Instant::now();
        let client = Client::connect(endpoint).await?;
        let events = client.subscribe(Some(after)).await?;
        elapsed = started.elapsed();
        *slot = Some(PersistentProjectStream {
            _client: client,
            events,
            identity: identity.to_owned(),
        });
    }
    slot.as_mut()
        .map(|stream| (elapsed, stream))
        .ok_or_else(|| Error::Invalid("Missing project event stream".into()))
}

async fn drain_project_stream(
    stream: &mut PersistentProjectStream,
    after: u64,
    tip: u64,
    data_dir: &str,
) -> Result<(Vec<Entry>, Vec<(u64, u64)>, u64), Error> {
    let mut records = Vec::new();
    let mut ledger_seqs = Vec::new();
    let mut reached = after;
    loop {
        let timeout = if reached < tip {
            Duration::from_secs(30)
        } else {
            Duration::from_millis(100)
        };
        let event = match tokio::time::timeout(timeout, stream.events.recv()).await {
            Ok(Some(event)) => event,
            Ok(None) => return Err(rosen_client::Error::Disconnected.into()),
            Err(_) if reached < tip => {
                return Err(Error::Invalid(
                    "Rosen event replay timed out before its watermark".into(),
                ));
            }
            Err(_) => break,
        };
        reached = reached.max(event.seq);
        let (projected, ledger_seq) = event_entries_with_ledger(&event)?;
        if let Some(seq) = ledger_seq {
            ledger_seqs.push((seq, event.seq));
        }
        records.extend(projected);
        if tip != 0 && reached >= tip {
            break;
        }
    }
    if !records.is_empty() {
        apply_server_timestamps(data_dir, after, &mut records).await;
    }
    Ok((records, ledger_seqs, reached))
}

fn commit_project_delta(
    state: &EndpointState,
    records: Vec<Entry>,
    ledger_seqs: Vec<(u64, u64)>,
    new_tip: u64,
) {
    let mut cache = state
        .cache
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let filtered: Vec<Entry> = records
        .into_iter()
        .filter(|record| record.sequence > cache.tip)
        .collect();
    cache.push_all(filtered, ledger_seqs);
    cache.tip = cache.tip.max(new_tip);
    cache.synced_once = true;
}

async fn resolve_stream_identity(
    endpoint: &Endpoint,
    state: &EndpointState,
) -> Result<(String, String), Error> {
    {
        let cache = state
            .cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let (Some(dir), Some(id)) = (&cache.data_dir, &cache.identity) {
            return Ok((dir.clone(), id.clone()));
        }
    }
    let client = velocity_rosen::connection::requests(endpoint).await?;
    let status = client.status().await?;
    let identity = database_identity(&status.data_dir);
    let mut cache = state
        .cache
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if cache.identity.as_deref() != Some(identity.as_str()) {
        cache.reset();
        cache.identity = Some(identity.clone());
    }
    cache.data_dir = Some(status.data_dir.clone());
    Ok((status.data_dir, identity))
}

async fn wait_for_ledger_event(
    endpoint: &Endpoint,
    state: &EndpointState,
    ledger_seq: u64,
) -> Result<(u64, Duration), Error> {
    let mut stream_guard = state.project_stream.lock().await;
    if let Some(seq) = state
        .cache
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .ledger_event_seq
        .get(&ledger_seq)
        .copied()
    {
        return Ok((seq, Duration::ZERO));
    }
    let (data_dir, identity) = resolve_stream_identity(endpoint, state).await?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let start_after = state
        .cache
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .tip;
    let (mut total_connect, mut reached, mut reconnected) = (Duration::ZERO, start_after, false);
    let (mut records, mut ledger_seqs) = (Vec::new(), Vec::new());
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            if !records.is_empty() {
                apply_server_timestamps(&data_dir, start_after, &mut records).await;
                commit_project_delta(state, records, ledger_seqs, reached);
            }
            return Err(Error::Invalid(format!(
                "Rosen record {ledger_seq} is stored, but its event was not received within 30 seconds; \
                 read the channel before retrying"
            )));
        }
        let (elapsed, stream) =
            ensure_project_stream(endpoint, &mut stream_guard, &identity, reached).await?;
        total_connect = total_connect.saturating_add(elapsed);
        match tokio::time::timeout(remaining, stream.events.recv()).await {
            Ok(Some(event)) => {
                reached = reached.max(event.seq);
                let (projected, found_ledger) = event_entries_with_ledger(&event)?;
                if let Some(seq) = found_ledger {
                    ledger_seqs.push((seq, event.seq));
                }
                records.extend(projected);
                if found_ledger == Some(ledger_seq) {
                    if !records.is_empty() {
                        apply_server_timestamps(&data_dir, start_after, &mut records).await;
                    }
                    commit_project_delta(state, records, ledger_seqs, reached);
                    return Ok((event.seq, total_connect));
                }
            }
            Ok(None) if !reconnected => {
                *stream_guard = None;
                reconnected = true;
            }
            Ok(None) => return Err(rosen_client::Error::Disconnected.into()),
            Err(_) => {}
        }
    }
}

async fn read_targeted_persistent(
    endpoint: &Endpoint,
    state: &EndpointState,
    nick: &str,
    after: u64,
    tip: u64,
    data_dir: &str,
) -> Result<(Vec<Entry>, Duration), Error> {
    let mut guard = state.targeted_client.lock().await;
    let mut total_connect = Duration::ZERO;
    for attempt in 0..2 {
        if guard.is_none() {
            let started = Instant::now();
            *guard = Some(Client::connect(endpoint).await?);
            total_connect = total_connect.saturating_add(started.elapsed());
        }
        let client = guard
            .as_ref()
            .ok_or_else(|| Error::Invalid("Missing targeted Rosen client".into()))?;
        match read_events_with_status(client, Some(nick), after, tip, data_dir).await {
            Ok(records) => return Ok((records, total_connect)),
            Err(error) if attempt == 0 && is_transport_error(&error) => {
                *guard = None;
            }
            Err(error) => {
                if is_transport_error(&error) {
                    *guard = None;
                }
                return Err(error);
            }
        }
    }
    Err(rosen_client::Error::Disconnected.into())
}

async fn apply_server_timestamps(data_dir: &str, after: u64, records: &mut [Entry]) {
    let data_dir = PathBuf::from(data_dir);
    let (floor, server_times) =
        tokio::task::spawn_blocking(move || velocity_rosen::event_timestamps(&data_dir, after))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
    apply_timestamps(records, floor, &server_times);
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
            Duration::from_secs(30)
        } else {
            Duration::from_millis(100)
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
    if !records.is_empty() {
        apply_server_timestamps(data_dir, after, &mut records).await;
    }
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
    let mut owner_changes = BTreeMap::new();
    for change in &changes {
        let id = change.change.id.to_string();
        let owner = change.change.owner.to_string();
        nicks.entry(id.clone()).or_insert_with(|| owner.clone());
        owner_changes.insert(owner, id.clone());
        if let Some(nick) = nicks.get(&id) {
            owner_changes.insert(nick.clone(), id);
        }
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
        if record.change.is_none()
            && is_change_related(&record.kind)
            && !matches!(record.nick.as_str(), "" | "velocity" | "you")
            && let Some(change) = owner_changes.get(&record.nick)
        {
            record.change = Some(change.clone());
            if let Some(claim) = &mut record.claim
                && claim.change.is_none()
            {
                claim.change = Some(change.clone());
            }
        }
        let Some(nick) = record.change_id().and_then(|change| nicks.get(change)) else {
            continue;
        };
        if record.payload.is_some() || record.kind == "checkout" {
            record.nick = nick.clone();
        }
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
