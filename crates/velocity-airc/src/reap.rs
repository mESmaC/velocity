//! Reaping dead local AIRC members and releasing pieces on lifecycle end (#566).
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Channel, Claim, Error,
    channel::{entry, presence},
    claims::{claims, member_change},
    remote::{Remote, classify_local_member, local_hostname},
    storage::Storage,
};

/// A held AIRC piece paired with whether its holder belongs to this machine (#566).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedClaim {
    pub claim: Claim,
    /// `true` for members scoped to this installation (or local unscoped members);
    /// `false` for members on other machines, which keep remote-work rules.
    pub local: bool,
}

/// Summary of an AIRC piece released when its local holder ended or was reaped (#566).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReapedClaim {
    pub piece: String,
    pub member: String,
    pub nick: String,
    pub task: String,
}

impl Channel {
    /// Lists held claims with machine locality so dispatch can ignore dead local claims
    /// while preserving claims owned by other machines (#566).
    pub fn scoped_claims(&self) -> Result<Vec<ScopedClaim>, Error> {
        if let Some(remote) = &self.remote {
            return remote.scoped_claims();
        }
        Ok(self
            .claims()?
            .into_iter()
            .map(|claim| {
                let local = is_local_file_member(&claim.member);
                ScopedClaim { claim, local }
            })
            .collect())
    }

    /// Releases every piece held by `member` and leaves the AIRC channel for it (#566).
    /// Bypasses quarantine freeze because Velocity itself is ending the member's lifecycle.
    pub fn release_member_and_leave(
        &self,
        member: &str,
        note: Option<&str>,
    ) -> Result<Vec<ReapedClaim>, Error> {
        self.release_matching_and_leave(|candidate| candidate == member, note)
    }

    /// Releases every piece held by any local member matching `matches_local` and leaves
    /// the channel for every joined matching local member (#566).
    pub fn release_matching_and_leave(
        &self,
        matches_local: impl Fn(&str) -> bool,
        note: Option<&str>,
    ) -> Result<Vec<ReapedClaim>, Error> {
        if let Some(remote) = &self.remote {
            return remote.release_matching_and_leave(matches_local);
        }
        let storage = Storage::open(self)?;
        let mut entries = storage.read()?;
        let reaped = release_file_claims(&storage, &mut entries, &matches_local, note)?;
        leave_file_members(&storage, &mut entries, &matches_local)?;
        Ok(reaped)
    }

    /// Reaps every local member that `is_live_local` reports as dead: releases all its
    /// pieces and leaves the channel so no dead local member holds pieces or presence (#566).
    /// Members on other machines are left untouched.
    pub fn reap_dead_members(
        &self,
        is_live_local: impl Fn(&str) -> bool,
    ) -> Result<Vec<ReapedClaim>, Error> {
        self.release_matching_and_leave(
            |member| !is_live_local(member),
            Some("stale AIRC claim released"),
        )
    }
}

fn release_file_claims(
    storage: &Storage,
    entries: &mut Vec<crate::Entry>,
    matches_local: &impl Fn(&str) -> bool,
    note: Option<&str>,
) -> Result<Vec<ReapedClaim>, Error> {
    let mut reaped = Vec::new();
    for claim in claims(entries) {
        if !is_local_file_member(&claim.member) || !matches_local(&claim.member) {
            continue;
        }
        let text = note
            .map(str::to_owned)
            .unwrap_or_else(|| claim.task.clone());
        let mut record = entry(entries, "released", "velocity", &claim.nick, &text);
        record.change = claim
            .change
            .clone()
            .or_else(|| member_change(entries, &claim.member));
        record.claim = Some(claim.clone());
        storage.append(&record)?;
        entries.push(record);
        reaped.push(ReapedClaim {
            piece: claim.key().to_owned(),
            member: claim.member,
            nick: claim.nick,
            task: claim.task,
        });
    }
    Ok(reaped)
}

fn leave_file_members(
    storage: &Storage,
    entries: &mut Vec<crate::Entry>,
    matches_local: &impl Fn(&str) -> bool,
) -> Result<(), Error> {
    let joined: Vec<_> = presence(entries)
        .into_values()
        .filter(|known| {
            known.joined && is_local_file_member(&known.member) && matches_local(&known.member)
        })
        .collect();
    for known in joined {
        let record = entry(
            entries,
            "leave",
            &known.member,
            &known.nick,
            "left the channel",
        );
        storage.append(&record)?;
        entries.push(record);
    }
    Ok(())
}

fn is_local_file_member(member: &str) -> bool {
    !member.contains('/')
        && !member.starts_with("velocity-machine-")
        && !matches!(member, "you" | "velocity")
}

impl Remote {
    pub(crate) fn scoped_claims(&self) -> Result<Vec<ScopedClaim>, Error> {
        let scope = self.scope.clone();
        let hostname = local_hostname();
        self.run(move |client| async move {
            let (pieces, members) = tokio::try_join!(client.pieces(), client.channel_presence())?;
            let machines: BTreeMap<&str, &str> = members
                .iter()
                .map(|member| (member.member.as_str(), member.machine.as_str()))
                .collect();
            Ok(pieces
                .iter()
                .map(|held| {
                    let machine = machines.get(held.member.as_str()).copied();
                    let local_id = classify_local_member(&scope, &hostname, &held.member, machine);
                    let mut localized = held.clone();
                    let local = match local_id {
                        Some(id) => {
                            localized.member = id;
                            true
                        }
                        None => false,
                    };
                    ScopedClaim {
                        claim: crate::remote_events::claim(&localized, Vec::new(), None),
                        local,
                    }
                })
                .collect())
        })
    }

    pub(crate) fn release_matching_and_leave(
        &self,
        matches_local: impl Fn(&str) -> bool,
    ) -> Result<Vec<ReapedClaim>, Error> {
        let (pieces, members) = self.run(|client| async move {
            tokio::try_join!(client.pieces(), client.channel_presence()).map_err(Error::from)
        })?;
        let hostname = local_hostname();
        let (to_release, reaped, leave_list) =
            select_remote_targets(&self.scope, &hostname, &pieces, &members, &matches_local);
        if to_release.is_empty() && leave_list.is_empty() {
            return Ok(reaped);
        }
        self.run(move |client| async move {
            for (raw_member, piece) in to_release {
                client.release_piece(&raw_member, Some(&piece)).await?;
            }
            for raw_member in leave_list {
                client.channel_leave(&raw_member).await?;
            }
            Ok(())
        })?;
        Ok(reaped)
    }
}

type RemoteReleasePlan = (Vec<(String, String)>, Vec<ReapedClaim>, Vec<String>);

fn select_remote_targets(
    scope: &str,
    hostname: &str,
    pieces: &[rosen_client::protocol::channel::PieceClaim],
    members: &[rosen_client::protocol::channel::Member],
    matches_local: &impl Fn(&str) -> bool,
) -> RemoteReleasePlan {
    let machines: BTreeMap<&str, &str> = members
        .iter()
        .map(|member| (member.member.as_str(), member.machine.as_str()))
        .collect();
    let mut to_release = Vec::new();
    let mut reaped = Vec::new();
    for held in pieces {
        let machine = machines.get(held.member.as_str()).copied();
        let Some(local_id) = classify_local_member(scope, hostname, &held.member, machine) else {
            continue;
        };
        if !matches_local(&local_id) {
            continue;
        }
        to_release.push((held.member.clone(), held.piece.clone()));
        reaped.push(ReapedClaim {
            piece: held.piece.clone(),
            member: local_id,
            nick: held.nick.clone(),
            task: held.task.clone(),
        });
    }
    let mut to_leave = BTreeSet::new();
    for member in members {
        if member.left_at.is_some() {
            continue;
        }
        let Some(local_id) =
            classify_local_member(scope, hostname, &member.member, Some(&member.machine))
        else {
            continue;
        };
        if matches_local(&local_id) {
            to_leave.insert(member.member.clone());
        }
    }
    (to_release, reaped, to_leave.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageMode;

    // Risk (#566): dead local members (ended threads or scouts, even if quarantined)
    // must have their pieces released and leave the channel without disturbing live
    // local members or remote machine members.
    #[test]
    fn reap_dead_members_releases_pieces_and_leaves_channel()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("velocity-reap-dead-{}-{stamp}", std::process::id()));
        let channel = Channel::open(&directory, "project")?;
        channel.join("duty-scout-1", "scout-1")?;
        channel.claim_piece(
            "duty-scout-1",
            "issue:152",
            "Fix C6 ledger receipt scan (#152)",
            "review-scout-1",
            Vec::new(),
        )?;
        channel.join("thread-42", "old-thread-42")?;
        channel.claim_piece(
            "thread-42",
            "issue:153",
            "Old thread work (#153)",
            "vel/old",
            Vec::new(),
        )?;
        channel.set_message_mode("old-thread-42", MessageMode::Quarantined)?;
        channel.join("issue-154-99", "issue-154-99")?;
        channel.claim_piece(
            "issue-154-99",
            "issue:154",
            "Live thread work (#154)",
            "vel/live",
            Vec::new(),
        )?;

        let reaped = channel.reap_dead_members(|member| member == "issue-154-99")?;
        assert_eq!(reaped.len(), 2);
        assert!(reaped.iter().any(|item| item.piece == "issue:152"));
        assert!(reaped.iter().any(|item| item.piece == "issue:153"));

        let remaining = channel.claims()?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key(), "issue:154");

        let members = channel.presence()?;
        assert!(
            members
                .iter()
                .any(|member| member.member == "issue-154-99" && member.joined)
        );
        assert!(
            members
                .iter()
                .any(|member| member.member == "duty-scout-1" && !member.joined)
        );
        assert!(
            members
                .iter()
                .any(|member| member.member == "thread-42" && !member.joined)
        );

        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#566): locality classification must reap only this machine's members and
    // preserve remote-machine scoped and enrolled members.
    #[test]
    fn classify_local_member_distinguishes_local_and_remote_members() {
        let scope = "929813e9072724a3/";
        assert_eq!(
            classify_local_member(
                scope,
                "DEFINITELYNOTAI",
                "929813e9072724a3/duty-scout-1",
                Some("DEFINITELYNOTAI"),
            ),
            Some("duty-scout-1".to_owned())
        );
        assert_eq!(
            classify_local_member(
                scope,
                "DEFINITELYNOTAI",
                "1122334455667788/issue-152-7",
                Some("OTHERBOX"),
            ),
            None
        );
        assert_eq!(
            classify_local_member(scope, "DEFINITELYNOTAI", "duty-scout-1", Some("velocity")),
            Some("duty-scout-1".to_owned())
        );
        assert_eq!(
            classify_local_member(
                scope,
                "DEFINITELYNOTAI",
                "duty-agent-12",
                Some("enrolled-machine-uuid"),
            ),
            None
        );
        assert_eq!(
            classify_local_member(
                scope,
                "DEFINITELYNOTAI",
                "velocity-machine-abc",
                Some("DEFINITELYNOTAI"),
            ),
            None
        );
    }
}
