//! Linking a project's Rosen server to its Collapse repository (#427, #517).
//!
//! `[mirror] remote` sends landed history. The top-level `collapse` root sends everything
//! else Collapse shows and takes back: the state feed (Rosen R9), the ledger push (R13)
//! and the owner's relayed actions (R14). Both land in one `rosen.toml` change, and the
//! relay runs only the kinds this server's operator allowed, which Velocity sets as the
//! owner of the project's server.

use rosen_client::Error;
use rosen_client::protocol::RelayKind;
use rosen_core::ProjectConfig;
use toml_edit::{DocumentMut, Item, value};

use crate::connection::requests;
use crate::workspace::session;
use crate::workspace_settings::{
    SETTINGS_AGENT, WorkspaceSettingsError, invalid, read_config, table, write_config,
};
use crate::{ProjectServer, RosenError};

/// What `rosen.toml` names for the project's Collapse repository.
pub struct CollapseLink<'link> {
    /// `[mirror] remote`, the git remote every land is pushed to. `None` keeps the
    /// project's mirror as it is, for one that already mirrors to Collapse.
    pub remote: Option<&'link str>,
    /// The `collapse` root, `https://api.collapse.sh/<owner>/<repository>`.
    pub root: &'link str,
}

/// Proposes a change that names the link in `rosen.toml`, so Rosen pushes every later
/// land and sends its live state once it lands. `None` when the landed file already
/// names both.
pub async fn propose_collapse_link(
    server: &ProjectServer,
    link: &CollapseLink<'_>,
) -> Result<Option<String>, WorkspaceSettingsError> {
    let client = requests(server.endpoint()).await?;
    let session = session(server, SETTINGS_AGENT).await?;
    let record = session.new_change(None).await?;
    let change = record.change.id.to_string();
    let message = match link.remote {
        Some(_) => "Mirror landed history and live state to Collapse",
        None => "Send live state to Collapse",
    };
    let result = async {
        let source = read_config(&client, record.change.head).await?;
        let Some(updated) = edit_collapse_link(&source, link)? else {
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

/// The Collapse root the configuration in force names, or `None` while Rosen sends
/// Collapse no live state. A project without a base has no configuration yet.
pub async fn collapse_root(server: &ProjectServer) -> Result<Option<String>, RosenError> {
    let client = requests(server.endpoint()).await?;
    Ok(client.base_config().await?.collapse)
}

/// Lets the project's server run every action Collapse's owner pages offer: approve, land,
/// mark reviewed, save a configuration change and revoke a machine (Rosen #404, #405,
/// #409). Each still passes Rosen's own gates, recorded as `<account>@collapse`. Returns
/// the kinds now allowed. Rosen takes the setting only from the server's own machine.
pub async fn allow_collapse_relay(server: &ProjectServer) -> Result<Vec<&'static str>, Error> {
    let client = requests(server.endpoint()).await?;
    let setting = client.relay_set(RelayKind::ALL.to_vec()).await?;
    Ok(setting
        .offered()
        .into_iter()
        .map(RelayKind::as_str)
        .collect())
}
