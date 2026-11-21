//! Pending calls subscribe to Rosen releases, including across machine boundaries.
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

use crate::{
    Channel, Entry, Error,
    remote::{Remote, run_on_shared_runtime, wire},
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
            &crate::waits::active_waits(&self.snapshot()?),
            member,
            &other.member,
        )?;
        let change = self.change_for(member)?;
        let change_for_rpc = change.clone();
        let file_for_rpc = file.to_owned();
        self.run(move |client| async move {
            client
                .wait_paths(wire(
                    json!({"change":change_for_rpc,"paths":[file_for_rpc]}),
                )?)
                .await?;
            Ok(())
        })?;
        let mut record = self.record(
            "waits",
            "velocity",
            &own.nick,
            &format!("waiting for {holder} · shares {file}"),
        );
        record.change = Some(change);
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
            .snapshot()?
            .iter()
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
            })
            .cloned())
    }

    pub(crate) fn wait_state(
        &self,
        member: &str,
        dependency: &DependencyWait,
    ) -> Result<WaitState, Error> {
        let change = self.change_for(member)?;
        let file = dependency.file.clone();
        self.run(move |client| async move {
            let filter: ClaimFilter = wire(json!({"change":change}))?;
            let pending = client
                .claim_list(filter)
                .await?
                .iter()
                .any(|path| path.kind == ClaimKind::Wait && path.path == file);
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
        // Open one dedicated subscription before recording the wait so releases cannot be
        // missed, then run the wait loop on the caller's thread so no sync call re-enters
        // the shared Rosen runtime (#555).
        let endpoint = self.endpoint.clone();
        let (_connection, events) = run_on_shared_runtime(async move {
            let connection = velocity_rosen::connection::Connection::dedicated(&endpoint).await?;
            let after = connection.status().await?.last_event;
            let events = connection.subscribe(Some(after)).await?;
            Ok((connection, Arc::new(AsyncMutex::new(events))))
        })?;
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
                let ended =
                    crate::waits::holder_end(&self.snapshot()?, dependency).unwrap_or_else(|| {
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
            wait_for_change(&events, &cancelled, expires)?;
        }
    }
}

fn wait_for_change(
    events: &Arc<AsyncMutex<tokio::sync::mpsc::Receiver<rosen_client::protocol::Event>>>,
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
        let receiver = Arc::clone(events);
        let arrived = run_on_shared_runtime(async move {
            let mut guard = receiver.lock().await;
            match tokio::time::timeout(std::time::Duration::from_millis(250), guard.recv()).await {
                Ok(Some(_)) => Ok(true),
                Err(_) => Ok(false),
                Ok(None) => Err(rosen_client::Error::Disconnected.into()),
            }
        })?;
        if arrived {
            return Ok(());
        }
    }
}
