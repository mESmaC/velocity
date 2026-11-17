//! Piece locks and path coordination are decided by Rosen, never by a local replay.
use crate::{
    Claim, ClaimResult, Error,
    remote::{Remote, wire},
    remote_events::{claim, time},
};
use rosen_client::protocol::channel::PieceOutcome;
use rosen_client::protocol::{ClaimFilter, ClaimKind, ClaimOutcome, ClaimRequest};
use serde_json::json;

impl Remote {
    pub(crate) fn change_for(&self, member: &str) -> Result<String, Error> {
        if let Some(change) = self
            .read(0)?
            .iter()
            .rev()
            .find(|record| record.member == member && record.checkout.is_some())
            .and_then(|record| record.head.clone())
        {
            return Ok(change);
        }
        self.run(async |client| {
            let presence = client.channel_presence().await?;
            let nick = presence
                .iter()
                .find(|known| known.member == self.member(member))
                .map(|known| known.nick.as_str())
                .ok_or_else(|| Error::Invalid("Join the channel first".into()))?;
            let filter = wire(json!({"owner":nick}))?;
            if let Some(change) =
                client
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
        })
    }

    pub(crate) fn claims(&self) -> Result<Vec<Claim>, Error> {
        let records = self.read(0)?;
        self.run(async |client| {
            let paths = client.claim_list(ClaimFilter::default()).await?;
            let changes = client.change_list(wire(json!({}))?).await?;
            Ok(client
                .pieces()
                .await?
                .iter()
                .map(|held| {
                    let change = records
                        .iter()
                        .rev()
                        .find(|record| {
                            record.member == self.local_member(&held.member)
                                && record.checkout.is_some()
                        })
                        .and_then(|record| record.head.clone())
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
                    local.member = self.local_member(&local.member);
                    claim(&local, files)
                })
                .collect())
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
        let change = if files.is_empty() {
            None
        } else {
            Some(self.change_for(member)?)
        };
        self.run(async |client| {
            let outcome = client
                .claim_piece(
                    &self.member(member),
                    (!piece.is_empty()).then_some(piece),
                    task,
                    branch,
                )
                .await?;
            let (held, extended) = match outcome {
                PieceOutcome::Refused { holder } => {
                    return Ok(ClaimResult::Refused {
                        holder: holder.nick,
                        since: time(holder.since),
                    });
                }
                PieceOutcome::Granted { claim, extended } => (claim, extended),
            };
            if let Some(change) = change
                && let Some(refused) = self
                    .claim_files(&client, &change, &held, extended, &files)
                    .await?
            {
                return Ok(refused);
            }
            let mut local = held;
            local.member = self.local_member(&local.member);
            Ok(ClaimResult::Granted {
                claim: claim(&local, files),
            })
        })
    }

    async fn claim_files(
        &self,
        client: &rosen_client::Client,
        change: &str,
        held: &rosen_client::protocol::channel::PieceClaim,
        extended: bool,
        files: &[String],
    ) -> Result<Option<ClaimResult>, Error> {
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
        let theirs = velocity_graph::coordination::rosen_changes(
            client,
            &holder.held.change.to_string(),
            path,
        )
        .await;
        let detail = crate::claims::symbol_overlap(
            ours.get(&holder.requested).map_or(&[], Vec::as_slice),
            theirs.get(&holder.requested).map_or(&[], Vec::as_slice),
        );
        let mut record = self.record(
            "overlap",
            "velocity",
            &held.nick,
            &format!(
                "{} overlaps {} on {}{detail}; record a share or wait before editing",
                held.nick, holder.held.owner, holder.requested
            ),
        );
        record.to = vec![held.nick.clone(), holder.held.owner.to_string()];
        self.append_record(&record)?;
        Ok(Some(ClaimResult::Refused {
            holder: holder.held.owner.to_string(),
            since: time(holder.held.since),
        }))
    }

    pub(crate) fn release(&self, member: &str, task: &str, note: &str) -> Result<(), Error> {
        crate::routing::require_unfrozen(&self.read(0)?, member)?;
        let held = self.claims()?;
        let key = crate::piece_key(task);
        let Some(claim) = held
            .iter()
            .find(|claim| claim.member == member && (claim.key() == key || claim.task == task))
        else {
            return Ok(());
        };
        let change = if claim.files.is_empty() {
            None
        } else {
            Some(self.change_for(member)?)
        };
        self.run(async |client| {
            // Release paths first: if transport fails, retry still finds the held
            // piece and cannot strand an exclusive path behind a released piece.
            if let Some(change) = change
                && !client
                    .pieces()
                    .await?
                    .iter()
                    .any(|piece| piece.member == self.member(member) && piece.piece != claim.key())
            {
                client
                    .release_paths(wire(json!({"change":change,"paths":[]}))?)
                    .await?;
            }
            client
                .release_piece(&self.member(member), Some(claim.key()))
                .await?;
            let mut record = self.record("released", "velocity", &claim.nick, note);
            record.claim = Some(claim.clone());
            self.append_record(&record)?;
            Ok(())
        })
    }

    pub(crate) fn share(&self, member: &str, holder: &str, file: &str) -> Result<(), Error> {
        crate::routing::require_unfrozen(&self.read(0)?, member)?;
        self.require_holder(holder, file)?;
        let change = self.change_for(member)?;
        self.run(async |client| {
            client
                .share_paths(wire(json!({"change":change,"paths":[file]}))?)
                .await?;
            let presence = client.channel_presence().await?;
            let nick = presence
                .iter()
                .find(|known| known.member == self.member(member))
                .map(|known| known.nick.as_str())
                .ok_or_else(|| Error::Invalid("Join the channel first".into()))?;
            let mut record = self.record(
                "shares",
                "velocity",
                nick,
                &format!("shares {file} with {holder}"),
            );
            record.to.push(holder.into());
            self.append_record(&record)?;
            Ok(())
        })
    }

    pub(crate) fn require_holder(&self, holder: &str, file: &str) -> Result<Claim, Error> {
        let claim = self
            .claims()?
            .into_iter()
            .find(|claim| claim.nick == holder)
            .ok_or_else(|| Error::Invalid("The named holder has no claim on this file".into()))?;
        let change = self.change_for(&claim.member)?;
        self.run(async |client| {
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
