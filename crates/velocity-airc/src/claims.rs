//! Durable first-wins piece claims and advisory file overlaps (SPEC.md F14).
use crate::{Channel, Entry, Error, channel::entry, storage::Storage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Claim {
    pub member: String,
    pub nick: String,
    pub task: String,
    pub branch: String,
    pub files: Vec<String>,
    pub since: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ClaimResult {
    Granted { claim: Claim },
    Refused { holder: String, since: String },
}

/// The pure decision shared with the Lean fixture model. Reclaiming one's own piece
/// extends it; another member cannot replace it, including after leave or failure.
pub fn may_claim(holder: Option<&str>, requester: &str) -> bool {
    holder.is_none_or(|holder| holder == requester)
}

pub fn claims(entries: &[Entry]) -> Vec<Claim> {
    let mut held = BTreeMap::new();
    for record in entries {
        let Some(claim) = &record.claim else {
            continue;
        };
        match record.kind.as_str() {
            "claim" => {
                held.insert(claim.task.clone(), claim.clone());
            }
            "released" => {
                held.remove(&claim.task);
            }
            _ => {}
        }
    }
    held.into_values().collect()
}

impl Channel {
    pub fn claims(&self) -> Result<Vec<Claim>, Error> {
        Ok(claims(&self.read(0)?))
    }

    /// Read, decide and append under the same cross-process channel lock.
    pub fn claim(
        &self,
        member: &str,
        task: &str,
        branch: &str,
        files: Vec<String>,
    ) -> Result<ClaimResult, Error> {
        if task.trim().is_empty()
            || branch.trim().is_empty()
            || files.iter().any(|file| file.trim().is_empty())
        {
            return Err(Error::Invalid("Claims need a task and branch".into()));
        }
        let storage = Storage::open(&self.directory)?;
        let mut entries = storage.read()?;
        let nick = active_nick(&entries, member)?;
        let held = claims(&entries);
        let previous = held.iter().find(|claim| claim.task == task);
        if !may_claim(previous.map(|claim| claim.member.as_str()), member) {
            let holder = previous.ok_or_else(|| Error::Invalid("Missing holder".into()))?;
            let text = format!(
                "{nick} refused {task}; held by {} since {}",
                holder.nick, holder.since
            );
            let record = entry(&entries, "refused", "velocity", &nick, &text);
            storage.append(&record)?;
            return Ok(ClaimResult::Refused {
                holder: holder.nick.clone(),
                since: holder.since.clone(),
            });
        }
        let mut record = entry(
            &entries,
            "claim",
            "velocity",
            &nick,
            &format!("{task} on {branch}"),
        );
        let claim = build_claim(member, nick, task, branch, files, previous, &record.time);
        record.claim = Some(claim.clone());
        if previous == Some(&claim) {
            return Ok(ClaimResult::Granted { claim });
        }
        storage.append(&record)?;
        entries.push(record);
        post_overlaps(&storage, &mut entries, &held, previous, &claim)?;
        Ok(ClaimResult::Granted { claim })
    }

    pub fn release(&self, member: &str, task: &str) -> Result<(), Error> {
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        if let Some(claim) = claims(&entries)
            .into_iter()
            .find(|claim| claim.task == task)
        {
            if claim.member != member {
                return Err(Error::Invalid("Only the holder can release a claim".into()));
            }
            let mut record = entry(&entries, "released", "velocity", &claim.nick, task);
            record.claim = Some(claim);
            storage.append(&record)?;
        }
        Ok(())
    }

    pub fn share(&self, member: &str, holder: &str, file: &str) -> Result<(), Error> {
        let storage = Storage::open(&self.directory)?;
        let entries = storage.read()?;
        let nick = active_nick(&entries, member)?;
        let file = normalize_file(file);
        let held = claims(&entries);
        if !held
            .iter()
            .any(|claim| claim.nick == holder && claim.files.contains(&file))
        {
            return Err(Error::Invalid(
                "The named holder has no claim on this file".into(),
            ));
        }
        storage.append(&entry(
            &entries,
            "shares",
            "velocity",
            &nick,
            &format!("shares {file} with {holder}"),
        ))
    }

    /// Observed edits extend every piece this agent holds, without changing piece ownership.
    pub fn observe_file(&self, member: &str, file: &str) -> Result<(), Error> {
        for claim in self
            .claims()?
            .into_iter()
            .filter(|claim| claim.member == member)
        {
            let normalized = normalize_file(file);
            if !claim.files.contains(&format!("{normalized}::registration")) {
                self.claim(member, &claim.task, &claim.branch, vec![normalized])?;
            }
        }
        Ok(())
    }
}

fn active_nick(entries: &[Entry], member: &str) -> Result<String, Error> {
    entries
        .iter()
        .rev()
        .find(|entry| entry.member == member && matches!(entry.kind.as_str(), "join" | "leave"))
        .filter(|entry| entry.kind == "join")
        .map(|entry| entry.nick.clone())
        .ok_or_else(|| Error::Invalid("Join the channel before claiming".into()))
}

fn post_overlaps(
    storage: &Storage,
    entries: &mut Vec<Entry>,
    held: &[Claim],
    previous: Option<&Claim>,
    claim: &Claim,
) -> Result<(), Error> {
    for other in held.iter().filter(|other| other.member != claim.member) {
        for file in claim.files.iter().filter(|file| !shared_registration(file)) {
            if other.files.contains(file) && !previous.is_some_and(|old| old.files.contains(file)) {
                let text = format!(
                    "{} overlaps {} on {file}; record a share or wait before editing",
                    claim.nick, other.nick
                );
                let record = entry(entries, "overlap", "velocity", &claim.nick, &text);
                storage.append(&record)?;
                entries.push(record);
            }
        }
    }
    Ok(())
}

/// Claims use repository-relative paths. Preserve case: Windows directories may be
/// case sensitive, and folding would conflate distinct files on those checkouts.
pub fn normalize_file(file: &str) -> String {
    let path = file.replace('\\', "/");
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    parts.join("/")
}

fn generated(file: &str) -> bool {
    matches!(
        file,
        "Cargo.lock" | "ui/pnpm-lock.yaml" | "ui/src/lib/bindings.ts"
    )
}

fn shared_registration(file: &str) -> bool {
    file.ends_with("::registration")
        && (file.starts_with("src-tauri/src/lib.rs::")
            || file.starts_with("Cargo.toml::")
            || file.contains("/src/lib.rs::"))
}

#[cfg(test)]
mod tests {
    // Concurrent builders must never both acquire the same piece; durable ownership
    // must also survive reopening the channel, including an agent leaving it.
    #[test]
    fn concurrent_claim_is_durable() -> Result<(), Box<dyn std::error::Error>> {
        use crate::{Channel, ClaimResult};
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("velocity-claim-{}-{stamp}", std::process::id()));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut workers = Vec::new();
        for index in 0..8 {
            let directory = directory.clone();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || -> Result<bool, crate::Error> {
                let channel = Channel::open(&directory, "project")?;
                let member = format!("agent-{index}");
                channel.join(&member, &member)?;
                barrier.wait();
                let result = channel.claim(&member, "piece", "vel/piece", Vec::new())?;
                channel.leave(&member)?;
                Ok(matches!(result, ClaimResult::Granted { .. }))
            }));
        }
        let mut granted = 0;
        for worker in workers {
            if worker.join().map_err(|_| "claim worker panicked")?? {
                granted += 1;
            }
        }
        assert_eq!(granted, 1);
        assert_eq!(Channel::open(&directory, "project")?.claims()?.len(), 1);
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // A changed grant decision could give a second agent ownership of live work.
    #[test]
    fn lean_claim_decisions() -> Result<(), Box<dyn std::error::Error>> {
        for line in include_str!("../../../proofs/fixtures/claim-lock.tsv").lines() {
            let fields: Vec<_> = line.split('\t').collect();
            let holder = (fields[0] != "-").then_some(fields[0]);
            assert_eq!(
                super::may_claim(holder, fields[1]),
                fields[2].parse::<bool>()?
            );
        }
        Ok(())
    }
}

fn build_claim(
    member: &str,
    nick: String,
    task: &str,
    branch: &str,
    files: Vec<String>,
    previous: Option<&Claim>,
    time: &str,
) -> Claim {
    let mut paths = previous
        .map(|claim| claim.files.clone())
        .unwrap_or_default();
    paths.extend(
        files
            .into_iter()
            .map(|file| normalize_file(&file))
            .filter(|file| !generated(file)),
    );
    paths.sort();
    paths.dedup();
    Claim {
        member: member.into(),
        nick,
        task: task.into(),
        branch: branch.into(),
        files: paths,
        since: previous.map_or_else(|| time.into(), |claim| claim.since.clone()),
    }
}
