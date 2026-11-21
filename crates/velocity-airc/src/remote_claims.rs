//! Piece locks and path coordination are decided by Rosen, never by a local replay.
use crate::{
    Claim, ClaimResult, Entry, Error,
    claims::WorkClaims,
    remote::{Remote, wire},
    remote_events::{claim, time},
};
use rosen_client::protocol::channel::PieceOutcome;
use rosen_client::protocol::{ClaimFilter, ClaimKind, ClaimOutcome, ClaimRequest};
use serde_json::json;
use velocity_rosen::policy::MergedPaths;

enum ClaimWorkOutcome {
    Done(ClaimResult),
    Overlap {
        record: Box<Entry>,
        result: ClaimResult,
    },
}

impl Remote {
    pub(crate) fn change_for(&self, member: &str) -> Result<String, Error> {
        if let Some(change) = self.checkout_change(member)? {
            return Ok(change);
        }
        let scoped = self.member(member);
        self.run(move |client| async move { resolve_change(&client, &scoped).await })
    }

    fn checkout_change(&self, member: &str) -> Result<Option<String>, Error> {
        let checkouts = self.checkouts()?;
        Ok(checkouts
            .get(&self.member(member))
            .or_else(|| checkouts.get(member))
            .cloned())
    }

    pub(crate) fn claims(&self) -> Result<Vec<Claim>, Error> {
        Ok(self.work_claims(None)?.pieces)
    }

    pub(crate) fn work_claims(&self, requester: Option<&str>) -> Result<WorkClaims, Error> {
        let checkouts = self.checkouts()?;
        let scope = self.scope.clone();
        let current = requester.and_then(|member| {
            checkouts
                .get(&self.member(member))
                .or_else(|| checkouts.get(member))
                .cloned()
        });
        let has_requester = requester.is_some();
        self.run(move |client| async move {
            let local_member = |member: &str| -> String {
                member.strip_prefix(&scope).unwrap_or(member).to_owned()
            };
            let lookup_checkout = |member: &str| -> Option<&str> {
                let scoped = if member.contains('/') || matches!(member, "you" | "velocity") {
                    member.to_owned()
                } else {
                    format!("{scope}{member}")
                };
                checkouts
                    .get(&scoped)
                    .or_else(|| checkouts.get(member))
                    .map(String::as_str)
            };
            let (paths, held_pieces) =
                tokio::try_join!(client.claim_list(ClaimFilter::default()), client.pieces(),)?;
            let needs_changes = held_pieces
                .iter()
                .any(|held| lookup_checkout(&local_member(&held.member)).is_none());
            let changes = if needs_changes {
                client.change_list(wire(json!({}))?).await?
            } else {
                Vec::new()
            };
            let pieces = held_pieces
                .iter()
                .map(|held| {
                    let change = lookup_checkout(&local_member(&held.member))
                        .map(str::to_owned)
                        .or_else(|| {
                            changes
                                .iter()
                                .rev()
                                .find(|change| change.change.owner.as_str() == held.nick)
                                .map(|change| change.change.id.to_string())
                        });
                    let files = paths
                        .iter()
                        .filter(|path| {
                            path.kind != ClaimKind::Wait && Some(path.change.to_string()) == change
                        })
                        .map(|path| path.path.clone())
                        .collect();
                    let mut local = held.clone();
                    local.member = local_member(&local.member);
                    claim(&local, files, change)
                })
                .collect();
            let paths = paths
                .into_iter()
                .filter(|path| {
                    has_requester
                        && path.kind != ClaimKind::Wait
                        && Some(path.change.to_string()) != current
                })
                .map(|path| path.path)
                .collect();
            Ok(WorkClaims { pieces, paths })
        })
    }

    pub(crate) fn claim_work(
        &self,
        member: &str,
        piece: &str,
        task: &str,
        branch: &str,
        files: Vec<String>,
    ) -> Result<ClaimResult, Error> {
        let files: Vec<_> = files
            .into_iter()
            .filter(|file| crate::claims::repository_relative(file))
            .map(|file| crate::claims::normalize_file(&file))
            .collect();
        let checkout_change = if files.is_empty() {
            None
        } else {
            self.checkout_change(member)?
        };
        let scoped = self.member(member);
        let scope = self.scope.clone();
        let piece = (!piece.is_empty()).then(|| piece.to_owned());
        let task = task.to_owned();
        let branch = branch.to_owned();
        let outcome = self.run(move |client| async move {
            let exclusive_files = exclusive_paths_for(&client, &files).await;
            let change = if exclusive_files.is_empty() {
                None
            } else if let Some(change) = checkout_change {
                Some(change)
            } else {
                Some(resolve_change(&client, &scoped).await?)
            };
            let outcome = client
                .claim_piece(&scoped, piece.as_deref(), &task, &branch)
                .await?;
            let (held, extended) = match outcome {
                PieceOutcome::Refused { holder } => {
                    return Ok(ClaimWorkOutcome::Done(ClaimResult::Refused {
                        holder: holder.nick,
                        since: time(holder.since),
                    }));
                }
                PieceOutcome::Granted { claim, extended } => (claim, extended),
            };
            if let Some(change_id) = change.as_deref()
                && let Some((record, result)) =
                    claim_files(&client, change_id, &held, extended, &exclusive_files).await?
            {
                return Ok(ClaimWorkOutcome::Overlap {
                    record: Box::new(record),
                    result,
                });
            }
            let mut local = held;
            local.member = local
                .member
                .strip_prefix(&scope)
                .unwrap_or(&local.member)
                .to_owned();
            Ok(ClaimWorkOutcome::Done(ClaimResult::Granted {
                claim: claim(&local, exclusive_files, change),
            }))
        })?;
        match outcome {
            ClaimWorkOutcome::Done(result) => Ok(result),
            ClaimWorkOutcome::Overlap { record, result } => {
                self.append_record(&record)?;
                Ok(result)
            }
        }
    }

    pub(crate) fn release(&self, member: &str, task: &str, note: &str) -> Result<(), Error> {
        crate::routing::require_unfrozen(&self.snapshot()?, member)?;
        let held = self.claims()?;
        let key = crate::piece_key(task);
        let Some(claim) = held
            .iter()
            .find(|claim| claim.member == member && (claim.key() == key || claim.task == task))
        else {
            return Ok(());
        };
        let change = if claim.files.is_empty() {
            claim.change.clone()
        } else {
            Some(self.change_for(member)?)
        };
        let release_paths = !claim.files.is_empty();
        let scoped = self.member(member);
        let claim_key = claim.key().to_owned();
        let change_for_rpc = change.clone();
        self.run(move |client| async move {
            // Release paths first: if transport fails, retry still finds the held
            // piece and cannot strand an exclusive path behind a released piece.
            if release_paths
                && let Some(change) = &change_for_rpc
                && !client
                    .pieces()
                    .await?
                    .iter()
                    .any(|piece| piece.member == scoped && piece.piece != claim_key)
            {
                client
                    .release_paths(wire(json!({"change":change,"paths":[]}))?)
                    .await?;
            }
            client.release_piece(&scoped, Some(&claim_key)).await?;
            Ok(())
        })?;
        let mut record = self.record("released", "velocity", &claim.nick, note);
        record.change = change;
        record.claim = Some(claim.clone());
        self.append_record(&record)?;
        Ok(())
    }

    pub(crate) fn share(&self, member: &str, holder: &str, file: &str) -> Result<(), Error> {
        crate::routing::require_unfrozen(&self.snapshot()?, member)?;
        self.require_holder(holder, file)?;
        let change = self.change_for(member)?;
        let scoped = self.member(member);
        let change_for_rpc = change.clone();
        let file_for_rpc = file.to_owned();
        let nick = self.run(move |client| async move {
            client
                .share_paths(wire(
                    json!({"change":change_for_rpc,"paths":[file_for_rpc]}),
                )?)
                .await?;
            let presence = client.channel_presence().await?;
            presence
                .iter()
                .find(|known| known.member == scoped)
                .map(|known| known.nick.clone())
                .ok_or_else(|| Error::Invalid("Join the channel first".into()))
        })?;
        let mut record = self.record(
            "shares",
            "velocity",
            &nick,
            &format!("shares {file} with {holder}"),
        );
        record.change = Some(change);
        record.to.push(holder.into());
        self.append_record(&record)?;
        Ok(())
    }

    pub(crate) fn require_holder(&self, holder: &str, file: &str) -> Result<Claim, Error> {
        let claim = self
            .claims()?
            .into_iter()
            .find(|claim| claim.nick == holder)
            .ok_or_else(|| Error::Invalid("The named holder has no claim on this file".into()))?;
        let change = self.change_for(&claim.member)?;
        let file = file.to_owned();
        self.run(move |client| async move {
            let overlaps = client.overlaps(wire(json!({"paths":[file]}))?).await?;
            if !overlaps
                .overlaps
                .iter()
                .any(|overlap| overlap.change.to_string() == change)
            {
                return Err(Error::Invalid(
                    "The named holder has no claim on this file".into(),
                ));
            }
            Ok(claim)
        })
    }
}

async fn resolve_change(client: &rosen_client::Client, scoped: &str) -> Result<String, Error> {
    let presence = client.channel_presence().await?;
    let nick = presence
        .iter()
        .find(|known| known.member == scoped)
        .map(|known| known.nick.as_str())
        .ok_or_else(|| Error::Invalid("Join the channel first".into()))?;
    let filter = wire(json!({"owner":nick}))?;
    if let Some(change) = client
        .change_list(filter)
        .await?
        .into_iter()
        .rev()
        .find(|change| {
            matches!(
                change.change.state,
                rosen_client::ChangeState::Open | rosen_client::ChangeState::Ready
            )
        })
    {
        return Ok(change.change.id.to_string());
    }
    // Duty agents have no build workspace. Their paths still need an R6
    // owner; a server change gives them one without any machine-local lock.
    Ok(client
        .change_new(wire(json!({"owner":nick}))?)
        .await?
        .change
        .id
        .to_string())
}

async fn exclusive_paths_for(client: &rosen_client::Client, files: &[String]) -> Vec<String> {
    if files.is_empty() {
        return Vec::new();
    }
    // Rosen refuses exclusive claims on generated artifacts and registration lists
    // declared in the base's rosen.toml (#545).
    let merged = MergedPaths::read(client).await.unwrap_or_default();
    exclusive_claim_paths(files, &merged)
}

/// Filters out paths Rosen refuses for exclusive claims: generated artifacts and
/// registration lists from the base's `rosen.toml`, plus legacy AIRC `::registration`
/// markers (#545).
fn exclusive_claim_paths(files: &[String], merged: &MergedPaths) -> Vec<String> {
    files
        .iter()
        .filter(|file| !file.ends_with("::registration") && !merged.contains(file))
        .cloned()
        .collect()
}

async fn claim_files(
    client: &rosen_client::Client,
    change: &str,
    held: &rosen_client::protocol::channel::PieceClaim,
    extended: bool,
    files: &[String],
) -> Result<Option<(Entry, ClaimResult)>, Error> {
    let request: ClaimRequest = wire(json!({"change":change,"paths":files,"exclusive":true}))?;
    let result = client.claim_paths(request).await;
    if !matches!(result, Ok(ClaimOutcome::Granted { .. })) && !extended {
        client
            .release_piece(&held.member, Some(&held.piece))
            .await?;
    }
    let ClaimOutcome::Refused { holders } = result? else {
        return Ok(None);
    };
    let holder = holders
        .first()
        .ok_or_else(|| Error::Invalid("Rosen refused paths without a holder".into()))?;
    let path = std::slice::from_ref(&holder.requested);
    let ours = velocity_graph::coordination::rosen_changes(client, change, path).await;
    let theirs =
        velocity_graph::coordination::rosen_changes(client, &holder.held.change.to_string(), path)
            .await;
    let detail = crate::claims::symbol_overlap(
        ours.get(&holder.requested).map_or(&[], Vec::as_slice),
        theirs.get(&holder.requested).map_or(&[], Vec::as_slice),
    );
    let mut record = crate::channel::entry(
        &[],
        "overlap",
        "velocity",
        &held.nick,
        &format!(
            "{} overlaps {} on {}{detail}; record a share or wait before editing",
            held.nick, holder.held.owner, holder.requested
        ),
    );
    record.change = Some(change.to_owned());
    record.to = vec![held.nick.clone(), holder.held.owner.to_string()];
    Ok(Some((
        record,
        ClaimResult::Refused {
            holder: holder.held.owner.to_string(),
            since: time(holder.held.since),
        },
    )))
}

#[cfg(test)]
mod tests {
    use super::exclusive_claim_paths;
    use crate::Channel;
    use velocity_rosen::policy::MergedPaths;

    // Risk (#545): sending generated artifacts or registration lists from rosen.toml in an
    // exclusive Rosen path claim fails with `exclusive claims cannot cover generated files or
    // registration lists` and rolls back the piece claim.
    #[test]
    fn exclusive_claim_paths_skips_generated_and_registration_lists()
    -> Result<(), Box<dyn std::error::Error>> {
        let merged = MergedPaths::parse_toml(include_str!("../../../rosen.toml"))?;
        let requested = vec![
            "src-tauri/src/lib.rs".into(),
            "crates/velocity-core/src/lib.rs".into(),
            "crates/velocity-rosen/src/lib.rs".into(),
            "ui/src/lib/ipc.ts".into(),
            "Cargo.lock".into(),
            "ui/src/lib/bindings.ts".into(),
            "Cargo.toml::registration".into(),
            "crates/velocity-airc/src/remote_claims.rs".into(),
            "ui/src/lib/Home.svelte".into(),
        ];
        let filtered = exclusive_claim_paths(&requested, &merged);
        assert_eq!(
            filtered,
            vec![
                "crates/velocity-airc/src/remote_claims.rs".to_owned(),
                "ui/src/lib/Home.svelte".to_owned(),
            ]
        );
        Ok(())
    }

    // Risk (#545, #531): editing registration lists or ordinary files on a Rosen-backed
    // channel must never trigger a server round trip or an `exclusive claims cannot cover` error.
    #[test]
    fn observe_file_on_rosen_channel_skips_server_calls() -> Result<(), Box<dyn std::error::Error>>
    {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "velocity-observe-rosen-{}-{stamp}",
            std::process::id()
        ));
        let channel = Channel::at_endpoint(&directory, "tcp://127.0.0.1:1");
        for path in [
            "src-tauri/src/lib.rs",
            "crates/velocity-core/src/lib.rs",
            "crates/velocity-rosen/src/lib.rs",
            "ui/src/lib/ipc.ts",
            "crates/velocity-airc/src/remote_claims.rs",
        ] {
            channel.observe_file("issue-545", path)?;
        }
        Ok(())
    }
}
