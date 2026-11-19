use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rosen_client::protocol::{ErrorCode, ReadAnswer, SnapshotRequest};
use rosen_client::{Client, Error};
use rosen_core::ProjectConfig;
use rosen_core::project_config::{CONFIG_PATH, MAX_WARM_POOL_SIZE};
use serde::{Deserialize, Serialize};
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, value};

use crate::{ProjectServer, RosenError, connection::requests, workspace::session};

/// The agent the configuration changes Velocity proposes are owned by and landed as.
pub const SETTINGS_AGENT: &str = "velocity-settings";

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceSettingsError {
    #[error(transparent)]
    Rosen(#[from] RosenError),
    #[error(transparent)]
    Client(#[from] Error),
    #[error("The selected workspace is no longer registered")]
    MissingWorkspace,
    #[error("The landed warm pool or prepare settings changed. Reload them and retry.")]
    SettingsChanged,
    #[error("Invalid {CONFIG_PATH}: {0}")]
    Configuration(String),
    #[error("Could not store the configuration in Rosen change {change}: {source}")]
    Write {
        change: String,
        #[source]
        source: Error,
    },
    #[error(
        "Rosen saved the configuration edit in change {change} but could not name it: {source}"
    )]
    Name {
        change: String,
        #[source]
        source: Error,
    },
}

pub struct RegisteredWorkspace {
    pub change: String,
    pub path: PathBuf,
    pub state: String,
    pub kept: Option<String>,
}

pub async fn workspace_count(server: &ProjectServer) -> Result<usize, Error> {
    Ok(requests(server.endpoint())
        .await?
        .workspace_list()
        .await?
        .len())
}

pub async fn list(server: &ProjectServer) -> Result<Vec<RegisteredWorkspace>, Error> {
    let client = requests(server.endpoint()).await?;
    let mut rows = client.workspace_list().await?;
    rows.sort_by(|left, right| left.path.cmp(&right.path));
    let mut states = BTreeMap::new();
    let mut listed = Vec::with_capacity(rows.len());
    for row in rows {
        let change = row.change.to_string();
        let state = match states.get(&row.change) {
            Some(state) => *state,
            None => {
                let state = match client.change_show(&change).await {
                    Ok(record) => record.change.state.as_str(),
                    Err(Error::Server(error)) if error.code == ErrorCode::NotFound => "unknown",
                    Err(error) => return Err(error),
                };
                states.insert(row.change, state);
                state
            }
        };
        listed.push(RegisteredWorkspace {
            change,
            path: row.path,
            state: state.into(),
            kept: row.kept,
        });
    }
    Ok(listed)
}

pub async fn delete(
    server: &ProjectServer,
    change: &str,
    path: &Path,
) -> Result<(), WorkspaceSettingsError> {
    let client = requests(server.endpoint()).await?;
    let registered = client
        .workspace_list()
        .await?
        .into_iter()
        .any(|record| record.path == path && record.change.to_string() == change);
    if !registered {
        return Err(WorkspaceSettingsError::MissingWorkspace);
    }
    client.workspace_delete(path).await?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct WorkspacePreparation {
    pub warm_pool_size: u32,
    pub prepare_commands: Vec<String>,
}

impl From<ProjectConfig> for WorkspacePreparation {
    fn from(config: ProjectConfig) -> Self {
        Self {
            warm_pool_size: config.warm_pool.size,
            prepare_commands: config.prepare.commands,
        }
    }
}

#[derive(Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct LandedWorkspacePreparation {
    pub preparation: WorkspacePreparation,
    pub maximum_pool_size: u32,
    pub config_error: Option<String>,
}

pub async fn preparation(server: &ProjectServer) -> Result<LandedWorkspacePreparation, Error> {
    let client = requests(server.endpoint()).await?;
    let config = client.project_config(None).await?;
    Ok(LandedWorkspacePreparation {
        preparation: config.config.into(),
        maximum_pool_size: MAX_WARM_POOL_SIZE,
        config_error: config.error,
    })
}

pub async fn propose_preparation(
    server: &ProjectServer,
    expected: &WorkspacePreparation,
    edited: &WorkspacePreparation,
) -> Result<Option<String>, WorkspaceSettingsError> {
    if expected == edited {
        return Ok(None);
    }
    let client = requests(server.endpoint()).await?;
    let session = session(server, SETTINGS_AGENT).await?;
    let record = session.new_change(None).await?;
    let change = record.change.id.to_string();
    let result = write_preparation(&client, &change, record.change.head, expected, edited).await;
    if let Err(error) = &result
        && !matches!(
            error,
            WorkspaceSettingsError::Write { .. } | WorkspaceSettingsError::Name { .. }
        )
    {
        let _ = session.abandon(&change).await;
    }
    result.map(|()| Some(change))
}

async fn write_preparation(
    client: &Client,
    change: &str,
    starting_tree: rosen_core::ObjectId,
    expected: &WorkspacePreparation,
    edited: &WorkspacePreparation,
) -> Result<(), WorkspaceSettingsError> {
    let source = read_config(client, starting_tree).await?;
    let updated = edit_config(&source, expected, edited)?;
    write_config(client, change, updated, "Update workspace preparation").await
}

/// Proposes a change that points `[mirror] remote` at `remote` (#427), so Rosen pushes
/// every later land there once it lands. `None` when the landed file already names it.
pub async fn propose_mirror_remote(
    server: &ProjectServer,
    remote: &str,
) -> Result<Option<String>, WorkspaceSettingsError> {
    let client = requests(server.endpoint()).await?;
    let session = session(server, SETTINGS_AGENT).await?;
    let record = session.new_change(None).await?;
    let change = record.change.id.to_string();
    let result = async {
        let source = read_config(&client, record.change.head).await?;
        let Some(updated) = edit_mirror_remote(&source, remote)? else {
            return Ok(false);
        };
        write_config(
            &client,
            &change,
            updated,
            "Mirror landed history to Collapse",
        )
        .await?;
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

/// The configuration with `[mirror] remote` set, or `None` when it already is. Comments
/// and every other setting keep their text.
fn edit_mirror_remote(
    source: &str,
    remote: &str,
) -> Result<Option<String>, WorkspaceSettingsError> {
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(|error| invalid(error.to_string()))?;
    let current = document
        .get("mirror")
        .and_then(|mirror| mirror.get("remote"))
        .and_then(Item::as_str);
    if current == Some(remote) {
        return Ok(None);
    }
    table(&mut document, "mirror")?.insert("remote", value(remote));
    let updated = document.to_string();
    ProjectConfig::parse(&updated).map_err(|error| invalid(error.to_string()))?;
    Ok(Some(updated))
}

/// The configuration in `starting_tree`, or empty when the base has none. Reading the
/// new change's exact starting tree keeps a concurrent land from being overwritten by
/// settings from an older base.
async fn read_config(
    client: &Client,
    starting_tree: rosen_core::ObjectId,
) -> Result<String, WorkspaceSettingsError> {
    let config = client
        .workspace_read(&starting_tree.to_string(), CONFIG_PATH)
        .await;
    match config {
        Ok(ReadAnswer::File { mode, data, .. }) if matches!(mode.as_str(), "100644" | "100755") => {
            String::from_utf8(data).map_err(|error| invalid(error.to_string()))
        }
        Ok(_) => Err(invalid("expected a regular file")),
        Err(Error::Server(error)) if error.code == ErrorCode::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

/// Stores `updated` as the change's configuration and names the snapshot `message`.
async fn write_config(
    client: &Client,
    change: &str,
    updated: String,
    message: &str,
) -> Result<(), WorkspaceSettingsError> {
    let written = client
        .workspace_write(change, CONFIG_PATH, Some(updated.into_bytes()))
        .await
        .map_err(|source| WorkspaceSettingsError::Write {
            change: change.to_owned(),
            source,
        })?;
    let head = written.change.change.head;
    client
        .snapshot(SnapshotRequest {
            change: change.to_owned(),
            tree: head,
            from: Some(head),
            message: Some(message.into()),
        })
        .await
        .map_err(|source| WorkspaceSettingsError::Name {
            change: change.to_owned(),
            source,
        })?;
    Ok(())
}

fn edit_config(
    source: &str,
    expected: &WorkspacePreparation,
    edited: &WorkspacePreparation,
) -> Result<String, WorkspaceSettingsError> {
    let current = ProjectConfig::parse(source).map_err(|error| invalid(error.to_string()))?;
    if &WorkspacePreparation::from(current) != expected {
        return Err(WorkspaceSettingsError::SettingsChanged);
    }
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(|error| invalid(error.to_string()))?;
    if edited.warm_pool_size != expected.warm_pool_size {
        table(&mut document, "warm_pool")?.insert("size", value(i64::from(edited.warm_pool_size)));
    }
    if edited.prepare_commands != expected.prepare_commands {
        let commands: Array = edited.prepare_commands.iter().map(String::as_str).collect();
        table(&mut document, "prepare")?.insert("commands", value(commands));
    }
    let updated = document.to_string();
    ProjectConfig::parse(&updated).map_err(|error| invalid(error.to_string()))?;
    Ok(updated)
}

fn table<'a>(
    document: &'a mut DocumentMut,
    name: &str,
) -> Result<&'a mut dyn TableLike, WorkspaceSettingsError> {
    document
        .entry(name)
        .or_insert(Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or_else(|| invalid(format!("expected a [{name}] table")))
}

fn invalid(message: impl Into<String>) -> WorkspaceSettingsError {
    WorkspaceSettingsError::Configuration(message.into())
}
