//! Durable F14 waits. Native filesystem notifications wake pending MCP calls;
//! reading the log after subscribing closes the subscribe/merge race.
use crate::{
    Channel, Entry, Error, Event, channel::entry, claims::normalize_file, storage::Storage,
};
use notify::Watcher;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct Checkout {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repository: String,
    /// Git worktree path; omitted on Rosen workspaces (which use `workspace`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub worktree: String,
    /// Rosen workspace path; omitted on Git worktrees (which use `worktree`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub workspace: String,
    /// Rosen change ID for this workspace (#540).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
    /// Git branch name; omitted on Rosen workspaces.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub branch: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base: String,
}

impl Checkout {
    pub fn new(repository: String, worktree: String, branch: String, base: String) -> Self {
        Self {
            repository,
            worktree,
            workspace: String::new(),
            change: None,
            branch,
            base,
        }
    }

    pub fn rosen(
        repository: String,
        workspace: String,
        change: Option<String>,
        base: String,
    ) -> Self {
        Self {
            repository,
            worktree: String::new(),
            workspace,
            change,
            branch: String::new(),
            base,
        }
    }

    /// Directory path of the workspace or git worktree.
    pub fn workspace_path(&self) -> &str {
        if !self.workspace.is_empty() {
            &self.workspace
        } else {
            &self.worktree
        }
    }

    /// Converts a Git-era checkout record to Rosen terms (`workspace` and `change`,
    /// omitting `worktree` and `branch`).
    pub(crate) fn into_rosen(mut self, change: Option<String>) -> Self {
        if self.workspace.is_empty() && !self.worktree.is_empty() {
            self.workspace = std::mem::take(&mut self.worktree);
        } else {
            self.worktree.clear();
        }
        self.branch.clear();
        if self.change.is_none() {
            self.change = change;
        }
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct DependencyWait {
    pub member: String,
    pub nick: String,
    pub holder: String,
    pub holder_nick: String,
    pub file: String,
}

/// Starts the `unblocked` line of a wait whose branch update failed after the merge.
const UPDATE_FAILED: &str = "dependency merged; resolve the update failure before editing";

/// Where a member's dependency wait stands, for parking (SPEC.md §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitState {
    /// No wait is pending for the member.
    NotWaiting,
    /// The wait is pending and its holder has not merged, closed, failed or been deleted.
    Waiting,
    /// The holder's work ended, but nobody has continued the wait yet.
    HolderEnded,
}

/// A parked member's wait after [`Channel::wake_parked`]: the `unblocked` line it was
/// sent, and whether updating its branch failed so it must resolve that first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitEnded {
    pub message: String,
    pub update_failed: bool,
}

impl Channel {
    pub fn register_checkout(&self, member: &str, checkout: Checkout) -> Result<(), Error> {
        let storage = Storage::open(self)?;
        let entries = storage.snapshot()?;
        let metadata = Path::new(checkout.workspace_path()).join(".rosen/workspace.json");
        let rosen_change = if metadata.is_file() {
            let value: serde_json::Value = serde_json::from_slice(&std::fs::read(metadata)?)?;
            value
                .get("change")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        } else {
            None
        };
        let is_rosen =
            self.remote.is_some() || rosen_change.is_some() || !checkout.workspace.is_empty();
        let (text, checkout, change) = if is_rosen {
            let change = checkout.change.clone().or(rosen_change);
            (
                "thread workspace",
                checkout.into_rosen(change.clone()),
                change,
            )
        } else {
            ("thread checkout", checkout, None)
        };
        let mut record = entry(&entries, "checkout", member, member, text);
        record.change = change;
        record.checkout = Some(checkout);
        storage.append(&record)
    }

    pub fn dependency_waits(&self) -> Result<Vec<DependencyWait>, Error> {
        Ok(active_waits(&self.snapshot()?).into_values().collect())
    }

    pub fn begin_wait(&self, member: &str, holder: &str, file: &str) -> Result<Entry, Error> {
        if let Some(remote) = &self.remote {
            return remote.begin_wait(member, holder, file);
        }
        let storage = Storage::open(self)?;
        let entries = storage.snapshot()?;
        let claims = crate::claims::claims(&entries);
        let own = claims
            .iter()
            .find(|claim| claim.member == member)
            .ok_or_else(|| Error::Invalid("Claim your piece before waiting".into()))?;
        let file = normalize_file(file);
        let other = claims
            .iter()
            .find(|claim| claim.nick == holder && claim.files.contains(&file))
            .ok_or_else(|| Error::Invalid("The holder has no claim on this file".into()))?;
        let waits = active_waits(&entries);
        reject_cycle(&waits, member, &other.member)?;
        let dependency = DependencyWait {
            member: member.into(),
            nick: own.nick.clone(),
            holder: other.member.clone(),
            holder_nick: other.nick.clone(),
            file,
        };
        let mut record = entry(
            &entries,
            "waits",
            "velocity",
            &own.nick,
            &format!("waiting for {} · shares {}", other.nick, dependency.file),
        );
        record.dependency = Some(dependency);
        storage.append(&record)?;
        Ok(record)
    }

    /// Called from the runner's saved lifecycle, including dependencies without a PR.
    pub fn dependency_ended(
        &self,
        member: &str,
        outcome: &str,
        number: Option<u32>,
    ) -> Result<(), Error> {
        let storage = Storage::open(self)?;
        let entries = storage.snapshot()?;
        if entries
            .iter()
            .rev()
            .filter(|entry| entry.member == member)
            .take_while(|entry| entry.checkout.is_none())
            .find(|entry| entry.kind == "dependency_end")
            .is_some_and(|entry| entry.text == outcome && entry.pull_request == number)
        {
            return Ok(());
        }
        let mut record = entry(&entries, "dependency_end", member, member, outcome);
        record.pull_request = number;
        storage.append(&record)
    }

    pub fn wait_and_continue(
        &self,
        member: &str,
        holder: &str,
        file: &str,
    ) -> Result<serde_json::Value, Error> {
        self.wait_until(member, holder, file, || false)
    }

    pub(crate) fn wait_until(
        &self,
        member: &str,
        holder: &str,
        file: &str,
        cancelled: impl Fn() -> bool + Sync,
    ) -> Result<serde_json::Value, Error> {
        if let Some(remote) = &self.remote {
            return remote.wait_until(self, member, holder, file, cancelled);
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                if result.as_ref().map_or(true, |event| {
                    (event.kind.is_modify() || event.kind.is_create() || event.kind.is_remove())
                        && event.paths.iter().any(|path| {
                            path.extension()
                                .is_some_and(|extension| extension == "jsonl")
                        })
                }) {
                    let _ = sender.send(result.map(|_| ()).map_err(|error| error.to_string()));
                }
            })
            .map_err(|error| Error::Invalid(error.to_string()))?;
        watcher
            .watch(&self.directory, notify::RecursiveMode::NonRecursive)
            .map_err(|error| Error::Invalid(error.to_string()))?;
        let waiting = self.begin_wait(member, holder, file)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20 * 60);
        let dependency = waiting
            .dependency
            .as_ref()
            .ok_or_else(|| Error::Invalid("Missing wait".into()))?;
        loop {
            if cancelled() {
                return Err(Error::Invalid("AIRC connection ended while waiting".into()));
            }
            let ended = holder_end(&self.snapshot()?, dependency);
            if let Some(ended) = ended {
                return self.continue_wait(member, dependency, &ended);
            }
            receive_change(&receiver, &cancelled, deadline)?;
        }
    }

    pub(crate) fn continue_wait(
        &self,
        member: &str,
        dependency: &DependencyWait,
        ended: &Entry,
    ) -> Result<serde_json::Value, Error> {
        let mut result = Ok(());
        if ended.text == "merged" {
            result = self.update_checkout(member);
        }
        let text = match &result {
            Ok(()) => format!(
                "unblocked · {} {}{}",
                dependency.holder_nick,
                ended.text,
                ended
                    .pull_request
                    .map_or(String::new(), |number| format!(" #{number}"))
            ),
            Err(error) => format!("{UPDATE_FAILED}: {error}"),
        };
        let storage = Storage::open(self)?;
        let entries = storage.snapshot()?;
        let mut record = entry(&entries, "unblocked", "velocity", &dependency.nick, &text);
        record.dependency = Some(dependency.clone());
        record.pull_request = ended.pull_request;
        storage.append(&record)?;
        drop(storage);
        if let Err(error) = result {
            self.post_event(Event::new("conflict", &dependency.nick, &text))?;
            return Err(error);
        }
        Ok(
            serde_json::json!({"outcome":ended.text,"message":text,"branch_updated":ended.text == "merged"}),
        )
    }

    pub fn wait_state(&self, member: &str) -> Result<WaitState, Error> {
        let entries = self.snapshot()?;
        let Some(dependency) = active_waits(&entries).remove(member) else {
            return Ok(WaitState::NotWaiting);
        };
        if let Some(remote) = &self.remote {
            if holder_end(&entries, &dependency).is_some() {
                return Ok(WaitState::HolderEnded);
            }
            return remote.wait_state(member, &dependency);
        }
        Ok(match holder_end(&entries, &dependency) {
            Some(_) => WaitState::HolderEnded,
            None => WaitState::Waiting,
        })
    }

    /// Ends a parked member's wait the way its pending `airc_wait` call would have: on a
    /// merge the branch is updated, and `unblocked` is posted. Its session was ended, so
    /// Velocity does this before resuming it. `None` while the holder's work is still open.
    /// A wait the call already ended before the session stopped is reported as it ended.
    pub fn wake_parked(&self, member: &str) -> Result<Option<WaitEnded>, Error> {
        let entries = self.snapshot()?;
        let mut failure = None;
        if let Some(dependency) = active_waits(&entries).remove(member) {
            let ended = if let Some(remote) = &self.remote {
                if self.wait_state(member)? != WaitState::HolderEnded {
                    return Ok(None);
                }
                holder_end(&entries, &dependency).unwrap_or_else(|| {
                    remote.record(
                        "dependency_end",
                        &dependency.holder,
                        &dependency.holder_nick,
                        "dependency released",
                    )
                })
            } else {
                let Some(ended) = holder_end(&entries, &dependency) else {
                    return Ok(None);
                };
                ended
            };
            drop(entries);
            failure = self.continue_wait(member, &dependency, &ended).err();
        }
        let unblocked = self
            .snapshot()?
            .iter()
            .rev()
            .find(|entry| {
                entry.kind == "unblocked"
                    && entry
                        .dependency
                        .as_ref()
                        .is_some_and(|dependency| dependency.member == member)
            })
            .cloned();
        match (unblocked, failure) {
            (Some(entry), _) => Ok(Some(WaitEnded {
                update_failed: entry.text.starts_with(UPDATE_FAILED),
                message: entry.text,
            })),
            (None, Some(error)) => Err(error),
            (None, None) => Err(Error::Invalid(format!("{member} has no ended wait"))),
        }
    }

    fn update_checkout(&self, member: &str) -> Result<(), Error> {
        let checkout = self
            .snapshot()?
            .iter()
            .rev()
            .find(|entry| entry.member == member && entry.checkout.is_some())
            .and_then(|entry| entry.checkout.clone())
            .ok_or_else(|| {
                Error::Invalid("Runner did not register this thread's checkout".into())
            })?;
        if Path::new(checkout.workspace_path()).join(".rosen").exists() {
            // Rosen rebases stacked children when the parent lands. Velocity applies
            // the pending head at the session's step boundary, never through git.
            return Ok(());
        }
        velocity_git::continue_after_dependency(
            Path::new(&checkout.repository),
            Path::new(checkout.workspace_path()),
            &checkout.branch,
            &checkout.base,
        )
        .map(|_branch| ())
        .map_err(|error| Error::Invalid(error.to_string()))
    }
}

pub(crate) fn active_waits(entries: &[Entry]) -> BTreeMap<String, DependencyWait> {
    let mut waits = BTreeMap::new();
    for entry in entries {
        if let Some(dependency) = &entry.dependency {
            if entry.kind == "waits" {
                waits.insert(dependency.member.clone(), dependency.clone());
            } else if entry.kind == "unblocked" {
                waits.remove(&dependency.member);
            }
        }
    }
    waits
}

/// The holder's `dependency_end`, counting only ends after its current checkout so an
/// earlier run of the same member never ends a new wait.
pub(crate) fn holder_end(entries: &[Entry], dependency: &DependencyWait) -> Option<Entry> {
    let checkout = entries
        .iter()
        .rev()
        .find(|entry| entry.member == dependency.holder && entry.checkout.is_some())
        .map_or(0, |entry| entry.sequence);
    entries
        .iter()
        .rev()
        .find(|entry| {
            entry.kind == "dependency_end"
                && entry.member == dependency.holder
                && entry.sequence > checkout
        })
        .cloned()
        .or_else(|| expired_wait(entries, dependency))
}

// Read the durable wait timestamp so restarting Velocity cannot reset the deadline.
fn expired_wait(entries: &[Entry], dependency: &DependencyWait) -> Option<Entry> {
    let waiting = entries.iter().rev().find(|entry| {
        entry.kind == "waits"
            && entry
                .dependency
                .as_ref()
                .is_some_and(|wait| wait.member == dependency.member)
    })?;
    let started = chrono::DateTime::parse_from_rfc3339(&waiting.time).ok()?;
    if chrono::Utc::now()
        .signed_duration_since(started)
        .num_seconds()
        < 20 * 60
    {
        return None;
    }
    let mut ended = waiting.clone();
    ended.text = "wait expired after 20 minutes; coordinate with the holder or proceed".into();
    Some(ended)
}

pub(crate) fn reject_cycle(
    waits: &BTreeMap<String, DependencyWait>,
    member: &str,
    holder: &str,
) -> Result<(), Error> {
    let mut next = holder;
    for _ in 0..=waits.len() {
        if next == member {
            return Err(Error::Invalid("Dependency wait would form a cycle".into()));
        }
        let Some(wait) = waits.get(next) else {
            return Ok(());
        };
        next = &wait.holder;
    }
    Err(Error::Invalid(
        "Dependency wait graph contains a cycle".into(),
    ))
}

// Check cancellation in short slices, but re-read dependency state only on events or expiry.
fn receive_change(
    receiver: &std::sync::mpsc::Receiver<Result<(), String>>,
    cancelled: &impl Fn() -> bool,
    deadline: std::time::Instant,
) -> Result<(), Error> {
    loop {
        if cancelled() {
            return Err(Error::Invalid("AIRC connection ended while waiting".into()));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(());
        }
        match receiver.recv_timeout(std::time::Duration::from_millis(250)) {
            Ok(result) => return result.map_err(Error::Invalid),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(error) => return Err(Error::Invalid(error.to_string())),
        }
    }
}

// #260: a blocked dependency must not strand a parked thread indefinitely after restart.
#[cfg(test)]
#[test]
fn blocked_wait_expires_from_durable_timestamp() {
    let dependency = DependencyWait {
        member: "a".into(),
        nick: "a".into(),
        holder: "b".into(),
        holder_nick: "b".into(),
        file: "shared.rs".into(),
    };
    let mut waiting = entry(&[], "waits", "velocity", "a", "waiting for b");
    waiting.dependency = Some(dependency.clone());
    assert!(expired_wait(&[waiting.clone()], &dependency).is_none());
    waiting.time = (chrono::Utc::now() - chrono::Duration::minutes(21)).to_rfc3339();
    assert!(
        holder_end(&[waiting], &dependency)
            .is_some_and(|ended| ended.text.contains("wait expired"))
    );
}

pub(crate) fn dependency(
    member: &str,
    nick: &str,
    holder: &str,
    holder_nick: &str,
    file: &str,
) -> DependencyWait {
    DependencyWait {
        member: member.into(),
        nick: nick.into(),
        holder: holder.into(),
        holder_nick: holder_nick.into(),
        file: file.into(),
    }
}
