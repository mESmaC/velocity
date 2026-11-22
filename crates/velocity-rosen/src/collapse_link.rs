//! Linking a project's Rosen server to its Collapse repository (#427, #517, #576).
//!
//! `[mirror] remote` sends landed history. The top-level `collapse` root sends everything
//! else Collapse shows and takes back: the state feed (Rosen R9), the ledger push (R13)
//! and the owner's relayed actions (R14). Channel lines opt in separately through
//! `collapse_channel`. The relay runs only the kinds this server's operator allowed, which
//! Velocity keeps in project state and re-applies whenever it starts the server.

use std::time::{Duration, Instant};

use rosen_client::Error;
use rosen_client::protocol::{CollapseCheckStatus, RelayKind};
use rosen_core::ProjectConfig;
use toml_edit::{DocumentMut, Entry, Item, value};
use velocity_core::collapse::{
    CollapseCheckItemResult, CollapseCheckItemStatus, CollapseCheckSummary, CollapseFeedHealth,
    CollapseRelayKind,
};

use crate::connection::requests;
use crate::mirror::{mirror_lag, mirror_remote};
use crate::mirror_catch_up::{CatchUp, catch_up_from_mirror};
use crate::workspace::session;
use crate::workspace_settings::{
    SETTINGS_AGENT, WorkspaceSettingsError, invalid, read_config, table, write_config,
};
use crate::{ProjectServer, RosenError};

pub use crate::mirror_catch_up::{RemoteBaseComparison, probe_remote_against_base};

/// Outcome of waiting for Rosen's mirror worker to fast-forward a Collapse remote (#592).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorFastForwardOutcome {
    /// Rosen confirmed the mirror remote holds Rosen's current base commit.
    Synced { base_commit: Option<String> },
    /// This Rosen server is not marked as the mirror owner, so it cannot push.
    NotOwner,
    /// The remote rejected the push (for example non-fast-forward).
    Rejected { error: String },
    /// Timed out waiting for Rosen's background push to finish.
    StillPushing { error: Option<String> },
}

/// Runs `rosen collapse check` against `server` without mutating state (#592).
pub async fn collapse_check(server: &ProjectServer) -> Result<CollapseCheckSummary, RosenError> {
    let client = requests(server.endpoint()).await?;
    let report = client.collapse_check().await?;
    Ok(CollapseCheckSummary {
        ok: report.ok,
        mirror_remote: report.mirror_remote,
        collapse: report.collapse,
        ledger_endpoint: report.ledger_endpoint,
        mirror_owner: report.mirror_owner,
        mirror_proactive_auth: report.mirror_proactive_auth,
        checks: report
            .checks
            .into_iter()
            .map(|item| CollapseCheckItemResult {
                name: item.name,
                status: match item.status {
                    CollapseCheckStatus::Pass => CollapseCheckItemStatus::Pass,
                    CollapseCheckStatus::Moved => CollapseCheckItemStatus::Moved,
                    CollapseCheckStatus::Fail => CollapseCheckItemStatus::Fail,
                    CollapseCheckStatus::Skipped => CollapseCheckItemStatus::Skipped,
                },
                detail: item.detail,
                hint: item.hint,
            })
            .collect(),
    })
}

/// Wakes Rosen's mirror push worker (safe when the base is ahead of or equal to the remote)
/// and waits until Rosen records `expected_remote` pushed up to the base commit (#592).
pub async fn wait_for_mirror_fast_forward(
    server: &ProjectServer,
    expected_remote: &str,
    timeout: Duration,
) -> Result<MirrorFastForwardOutcome, RosenError> {
    // Asking Rosen to check catch-up when our base leads wakes its mirror push worker out of
    // idle or retry backoff immediately without rewriting history.
    if let Ok(CatchUp::Diverged(divergence)) = catch_up_from_mirror(server).await {
        return Ok(MirrorFastForwardOutcome::Rejected {
            error: divergence.detail,
        });
    }
    let deadline = Instant::now() + timeout;
    let mut last_error: Option<String> = None;
    loop {
        let remote = mirror_remote(server).await?;
        let remote_matches = remote
            .as_deref()
            .is_some_and(|current| current.eq_ignore_ascii_case(expected_remote));
        if remote_matches {
            match mirror_lag(server).await? {
                None => {
                    let base = read_current_base(server).await;
                    return Ok(MirrorFastForwardOutcome::Synced { base_commit: base });
                }
                Some(lag) if lag.not_owner => return Ok(MirrorFastForwardOutcome::NotOwner),
                Some(lag) if lag.rejected => {
                    return Ok(MirrorFastForwardOutcome::Rejected {
                        error: lag
                            .error
                            .unwrap_or_else(|| "mirror push was rejected".into()),
                    });
                }
                Some(lag) => {
                    if lag.error.is_some() {
                        last_error = lag.error;
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return Ok(MirrorFastForwardOutcome::StillPushing { error: last_error });
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn read_current_base(server: &ProjectServer) -> Option<String> {
    let reading = server.clone();
    tokio::task::spawn_blocking(move || crate::mirror_catch_up::read_base_ref(&reading))
        .await
        .ok()?
        .ok()?
        .map(|(_, commit)| commit)
}

/// What `rosen.toml` names for the project's Collapse repository.
pub struct CollapseLink<'link> {
    /// `[mirror] remote`, the git remote every land is pushed to. `None` keeps the
    /// project's mirror as it is, for one that already mirrors to Collapse.
    pub remote: Option<&'link str>,
    /// The `collapse` root, `https://api.collapse.sh/<owner>/<repository>`.
    pub root: &'link str,
}

/// Live Collapse configuration, feed health and relay state read from a project's server (#576).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollapseServerState {
    pub collapse_root: Option<String>,
    pub collapse_channel: bool,
    pub config_error: Option<String>,
    pub state_feed: CollapseFeedHealth,
    pub ledger_push: CollapseFeedHealth,
    pub relay_allow: Vec<CollapseRelayKind>,
    pub relay_revoked: bool,
}

/// Proposes a change that names the link in `rosen.toml`, so Rosen pushes every later
/// land and sends its live state once it lands. `None` when the landed file already
/// names both.
pub async fn propose_collapse_link(
    server: &ProjectServer,
    link: &CollapseLink<'_>,
) -> Result<Option<String>, WorkspaceSettingsError> {
    let message = match link.remote {
        Some(_) => "Mirror landed history and live state to Collapse",
        None => "Send live state to Collapse",
    };
    propose_config_edit(server, message, |source| edit_collapse_link(source, link)).await
}

/// Proposes a `rosen.toml` change toggling `collapse_channel` (#576). Returns `None` when
/// the landed file already has `enabled`.
pub async fn propose_collapse_channel(
    server: &ProjectServer,
    enabled: bool,
) -> Result<Option<String>, WorkspaceSettingsError> {
    let message = if enabled {
        "Send channel lines to Collapse"
    } else {
        "Stop sending channel lines to Collapse"
    };
    propose_config_edit(server, message, |source| {
        edit_collapse_channel(source, enabled)
    })
    .await
}

/// Proposes a `rosen.toml` change removing the top-level `collapse` root (and
/// `collapse_channel`), leaving `[mirror] remote` intact (#576). Returns `None` when
/// neither key is set.
pub async fn propose_collapse_unlink(
    server: &ProjectServer,
) -> Result<Option<String>, WorkspaceSettingsError> {
    propose_config_edit(server, "Unlink Collapse live state", edit_collapse_unlink).await
}

async fn propose_config_edit(
    server: &ProjectServer,
    message: &str,
    edit: impl FnOnce(&str) -> Result<Option<String>, WorkspaceSettingsError>,
) -> Result<Option<String>, WorkspaceSettingsError> {
    let client = requests(server.endpoint()).await?;
    let session = session(server, SETTINGS_AGENT).await?;
    let record = session.new_change(None).await?;
    let change = record.change.id.to_string();
    let result = async {
        let source = read_config(&client, record.change.head).await?;
        let Some(updated) = edit(&source)? else {
            return Ok(false);
        };
        write_config(&client, &change, updated, message).await?;
        Ok(true)
    }
    .await;
    match result {
        Ok(true) => Ok(Some(change)),
        Ok(false) => {
            let _ = session.abandon(&change).await;
            Ok(None)
        }
        Err(error) => {
            // A stored edit stays for a person to finish; anything earlier left nothing.
            if !matches!(
                error,
                WorkspaceSettingsError::Write { .. } | WorkspaceSettingsError::Name { .. }
            ) {
                let _ = session.abandon(&change).await;
            }
            Err(error)
        }
    }
}

/// The configuration with the link set, or `None` when it already is. Comments and every
/// other setting keep their text.
fn edit_collapse_link(
    source: &str,
    link: &CollapseLink<'_>,
) -> Result<Option<String>, WorkspaceSettingsError> {
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(|error| invalid(error.to_string()))?;
    let current_remote = document
        .get("mirror")
        .and_then(|mirror| mirror.get("remote"))
        .and_then(Item::as_str);
    let remote = link.remote.filter(|remote| current_remote != Some(*remote));
    let rooted = document.get("collapse").and_then(Item::as_str) == Some(link.root);
    if remote.is_none() && rooted {
        return Ok(None);
    }
    if let Some(remote) = remote {
        table(&mut document, "mirror")?.insert("remote", value(remote));
    }
    if !rooted {
        document.insert("collapse", value(link.root));
    }
    let updated = document.to_string();
    let parsed = ProjectConfig::parse(&updated).map_err(|error| invalid(error.to_string()))?;
    // A root key written below a table header would belong to that table. Rosen must
    // read back exactly the root asked for before the change is stored.
    if parsed.collapse.as_deref() != Some(link.root) {
        return Err(invalid("the collapse root did not land as a top-level key"));
    }
    Ok(Some(updated))
}

fn edit_collapse_channel(
    source: &str,
    enabled: bool,
) -> Result<Option<String>, WorkspaceSettingsError> {
    let current = ProjectConfig::parse(source).map_err(|error| invalid(error.to_string()))?;
    if current.collapse.is_none() {
        return Err(invalid(
            "cannot change channel lines without a collapse root",
        ));
    }
    if current.collapse_channel == enabled {
        return Ok(None);
    }
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(|error| invalid(error.to_string()))?;
    if enabled {
        let mut replacement = value(true);
        match document.entry("collapse_channel") {
            Entry::Occupied(mut entry) => {
                if let (Some(existing), Some(updated)) =
                    (entry.get().as_value(), replacement.as_value_mut())
                {
                    *updated.decor_mut() = existing.decor().clone();
                }
                entry.insert(replacement);
            }
            Entry::Vacant(entry) => {
                entry.insert(replacement);
            }
        }
    } else {
        document.remove("collapse_channel");
    }
    let updated = document.to_string();
    let parsed = ProjectConfig::parse(&updated).map_err(|error| invalid(error.to_string()))?;
    if parsed.collapse_channel != enabled {
        return Err(invalid("collapse_channel did not land as a top-level key"));
    }
    Ok(Some(updated))
}

fn edit_collapse_unlink(source: &str) -> Result<Option<String>, WorkspaceSettingsError> {
    let current = ProjectConfig::parse(source).map_err(|error| invalid(error.to_string()))?;
    if current.collapse.is_none() && !current.collapse_channel {
        return Ok(None);
    }
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(|error| invalid(error.to_string()))?;
    document.remove("collapse");
    document.remove("collapse_channel");
    let updated = document.to_string();
    let parsed = ProjectConfig::parse(&updated).map_err(|error| invalid(error.to_string()))?;
    if parsed.collapse.is_some() || parsed.collapse_channel {
        return Err(invalid("the collapse root could not be removed"));
    }
    Ok(Some(updated))
}

/// Reads the local Rosen server's Collapse state-feed identifier, if one has been minted (#619).
pub fn collapse_feed_id(server: &ProjectServer) -> Option<String> {
    crate::status::metadata::read_collapse_feed_id(server)
}

/// Reads the landed Collapse settings, state feed / ledger push health and active server
/// relay allow-list for `server` (#576, #662).
pub async fn collapse_state(server: &ProjectServer) -> Result<CollapseServerState, RosenError> {
    let client = requests(server.endpoint()).await?;
    let (record, status) = tokio::try_join!(client.project_config(None), client.status())?;
    let (state_feed, ledger_push) = match status.collapse.as_ref() {
        Some(collapse) => (
            crate::status::collapse_feed_health(&collapse.state_feed),
            crate::status::collapse_feed_health(&collapse.ledger_push),
        ),
        None => (CollapseFeedHealth::default(), CollapseFeedHealth::default()),
    };
    let (relay_allow, relay_revoked) = match status.relay {
        Some(relay) => (
            CollapseRelayKind::normalize(relay.allow.into_iter().map(from_relay_kind)),
            relay.revoked,
        ),
        None => {
            let owned = server.clone();
            tokio::task::spawn_blocking(move || crate::status::metadata::read_relay_setting(&owned))
                .await
                .map_err(|error| RosenError::StatusData(error.to_string()))??
        }
    };
    Ok(CollapseServerState {
        collapse_root: record.config.collapse,
        collapse_channel: record.config.collapse_channel,
        config_error: record.error,
        state_feed,
        ledger_push,
        relay_allow,
        relay_revoked,
    })
}

/// The Collapse root the configuration in force names, or `None` while Rosen sends
/// Collapse no live state. A project without a base has no configuration yet.
pub async fn collapse_root(server: &ProjectServer) -> Result<Option<String>, RosenError> {
    let client = requests(server.endpoint()).await?;
    Ok(client.base_config().await?.collapse)
}

/// Sets the action kinds the project's server runs from Collapse's owner pages (#576).
/// An empty slice turns the relay off. Returns the kinds now offered by the server.
/// Rosen takes the setting only from the server's own machine.
pub async fn set_collapse_relay(
    server: &ProjectServer,
    allow: &[CollapseRelayKind],
) -> Result<Vec<CollapseRelayKind>, Error> {
    let client = requests(server.endpoint()).await?;
    let requested: Vec<RelayKind> = CollapseRelayKind::normalize(allow.iter().copied())
        .into_iter()
        .map(to_relay_kind)
        .collect();
    let setting = client.relay_set(requested).await?;
    Ok(setting.offered().into_iter().map(from_relay_kind).collect())
}

fn to_relay_kind(kind: CollapseRelayKind) -> RelayKind {
    match kind {
        CollapseRelayKind::Approve => RelayKind::Approve,
        CollapseRelayKind::Land => RelayKind::Land,
        CollapseRelayKind::Mark => RelayKind::Mark,
        CollapseRelayKind::Config => RelayKind::Config,
        CollapseRelayKind::Revoke => RelayKind::Revoke,
    }
}

fn from_relay_kind(kind: RelayKind) -> CollapseRelayKind {
    match kind {
        RelayKind::Approve => CollapseRelayKind::Approve,
        RelayKind::Land => CollapseRelayKind::Land,
        RelayKind::Mark => CollapseRelayKind::Mark,
        RelayKind::Config => CollapseRelayKind::Config,
        RelayKind::Revoke => CollapseRelayKind::Revoke,
    }
}

#[cfg(test)]
mod tests {
    use super::{edit_collapse_channel, edit_collapse_unlink};
    use rosen_core::ProjectConfig;

    // Risk (#576): toggling collapse_channel or unlinking must edit only top-level Collapse
    // keys while keeping comments and [mirror] remote intact.
    #[test]
    fn collapse_channel_and_unlink_preserve_other_settings()
    -> Result<(), Box<dyn std::error::Error>> {
        let base = "# Comment\nreview = \"after-land\"\ncollapse = \"https://api.collapse.sh/o/r\"\n\n[mirror]\nremote = \"https://api.collapse.sh/o/r.git\"\n";
        let enabled = edit_collapse_channel(base, true)?.ok_or("expected collapse_channel edit")?;
        let parsed = ProjectConfig::parse(&enabled)?;
        assert!(parsed.collapse_channel);
        assert_eq!(
            parsed.collapse.as_deref(),
            Some("https://api.collapse.sh/o/r")
        );
        assert_eq!(
            parsed.mirror.remote.as_deref(),
            Some("https://api.collapse.sh/o/r.git")
        );
        assert!(enabled.contains("# Comment"));
        assert_eq!(edit_collapse_channel(&enabled, true)?, None);

        let disabled = edit_collapse_channel(&enabled, false)?
            .ok_or("expected collapse_channel disable edit")?;
        let parsed = ProjectConfig::parse(&disabled)?;
        assert!(!parsed.collapse_channel);
        assert_eq!(
            parsed.collapse.as_deref(),
            Some("https://api.collapse.sh/o/r")
        );

        let unlinked = edit_collapse_unlink(&enabled)?.ok_or("expected unlink edit")?;
        let parsed = ProjectConfig::parse(&unlinked)?;
        assert_eq!(parsed.collapse, None);
        assert!(!parsed.collapse_channel);
        assert_eq!(
            parsed.mirror.remote.as_deref(),
            Some("https://api.collapse.sh/o/r.git")
        );
        assert_eq!(edit_collapse_unlink(&unlinked)?, None);
        Ok(())
    }
}
