//! Pending calls subscribe to Rosen releases, including across machine boundaries.
use crate::{
    Channel, Entry, Error,
    remote::{Remote, wire},
    waits::{DependencyWait, WaitState},
};
use rosen_client::protocol::{ClaimFilter, ClaimKind};
use serde_json::json;

impl Remote {
    pub(crate) fn begin_wait(
        &self,
        member: &str,
        holder: &str,
        file: &str,
    ) -> Result<Entry, Error> {
        if let Some(record) = self.resumed_wait(member, holder, file)? {
            return Ok(record);
        }
        let other = self.require_holder(holder, file)?;
        let own = self
            .claims()?
            .into_iter()
            .find(|claim| claim.member == member)
            .ok_or_else(|| Error::Invalid("Claim your piece before waiting".into()))?;
        crate::waits::reject_cycle(
            &crate::waits::active_waits(&self.read(0)?),
            member,
            &other.member,
        )?;
        let change = self.change_for(member)?;
        self.run(async |client| {
            client
                .wait_paths(wire(json!({"change":change,"paths":[file]}))?)
                .await?;
            Ok(())
        })?;
        let mut record = self.record(
            "waits",
            "velocity",
            &own.nick,
            &format!("waiting for {holder} · shares {file}"),
        );
        record.dependency = Some(crate::waits::dependency(
            member,
            &own.nick,
            &other.member,
            holder,
            file,
        ));
        self.append_record(&record)
    }

    fn resumed_wait(&self, member: &str, holder: &str, file: &str) -> Result<Option<Entry>, Error> {
        Ok(self
            .read(0)?
            .into_iter()
            .rev()
            .find(|record| {
                record
                    .dependency
                    .as_ref()
                    .is_some_and(|wait| wait.member == member)
            })
            .filter(|record| {
                record.kind == "waits"
                    && record
                        .dependency
                        .as_ref()
                        .is_some_and(|wait| wait.holder_nick == holder && wait.file == file)
            }))
    }

    pub(crate) fn wait_state(
        &self,
        member: &str,
        dependency: &DependencyWait,
    ) -> Result<WaitState, Error> {
        let change = self.change_for(member)?;
        self.run(async |client| {
            let filter: ClaimFilter = wire(json!({"change":change}))?;
            let pending = client
                .claim_list(filter)
                .await?
                .iter()
                .any(|path| path.kind == ClaimKind::Wait && path.path == dependency.file);
            Ok(if pending {
                WaitState::Waiting
            } else {
                WaitState::HolderEnded
            })
        })
    }

    pub(crate) fn wait_until(
        &self,
        channel: &Channel,
        member: &str,
        holder: &str,
        file: &str,
        cancelled: impl Fn() -> bool + Sync,
    ) -> Result<serde_json::Value, Error> {
        self.run_dedicated(async |client| {
            // An explicit cursor also replays releases while Rosen starts its
            // subscription task; a ping cannot establish that task's starting point.
            let after = client.status().await?.last_event;
            let mut events = client.subscribe(Some(after)).await?;
            let record = self.begin_wait(member, holder, file)?;
            let started = chrono::DateTime::parse_from_rfc3339(&record.time)
                .map_err(|error| Error::Invalid(error.to_string()))?;
            let expires = started + chrono::Duration::minutes(20);
            let dependency = record
                .dependency
                .as_ref()
                .ok_or_else(|| Error::Invalid("Missing wait".into()))?;
            loop {
                if cancelled() {
                    return Err(Error::Invalid("AIRC connection ended while waiting".into()));
                }
                if channel.wait_state(member)? == WaitState::HolderEnded {
                    let ended = crate::waits::holder_end(&self.read(0)?, dependency)
                        .unwrap_or_else(|| {
                            self.record(
                                "dependency_end",
                                &dependency.holder,
                                &dependency.holder_nick,
                                "dependency released",
                            )
                        });
                    return channel.continue_wait(member, dependency, &ended);
                }
                // Cancellation and the durable 20 minute deadline must remain observable
                // even if a live server has nothing to publish.
                wait_for_change(&mut events, &cancelled, expires).await?;
            }
        })
    }
}

async fn wait_for_change(
    events: &mut tokio::sync::mpsc::Receiver<rosen_client::protocol::Event>,
    cancelled: &impl Fn() -> bool,
    expires: chrono::DateTime<chrono::FixedOffset>,
) -> Result<(), Error> {
    loop {
        if cancelled() {
            return Err(Error::Invalid("AIRC connection ended while waiting".into()));
        }
        if chrono::Utc::now() >= expires {
            return Ok(());
        }
        match tokio::time::timeout(std::time::Duration::from_millis(250), events.recv()).await {
            Ok(Some(_)) => return Ok(()),
            Err(_) => continue,
            Ok(None) => return Err(rosen_client::Error::Disconnected.into()),
        }
    }
}
