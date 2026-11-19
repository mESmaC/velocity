//! The GitHub workflows a project's owner lets agents request (#508), read from the
//! `rosen.toml` Rosen has in force at the base. Never from a workspace: an agent can edit
//! its own copy, and an edit counts only once it lands.

use rosen_client::protocol::{ErrorCode, ReadAnswer, ReadRequest};
use rosen_client::{Client, Endpoint, Error as ClientError};
use rosen_core::project_config::CONFIG_PATH;
use toml_edit::{DocumentMut, Item};

use crate::RosenError;

/// What the configuration in force says about dispatching workflows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkflowPolicy {
    /// `[ci] dispatchable`: workflow file names under `.github/workflows`.
    pub dispatchable: Vec<String>,
    /// `[mirror] remote`, the GitHub repository whose workflows these are.
    pub mirror: Option<String>,
    /// Why Rosen refused the base's own `rosen.toml`, when an older file is in force.
    pub refused: Option<String>,
}

/// Reads the policy from the server at `endpoint`, an agent's `ROSEN_ENDPOINT`.
pub async fn workflow_policy(endpoint: &str) -> Result<WorkflowPolicy, RosenError> {
    let client = crate::connection::requests(&Endpoint::new(endpoint)).await?;
    let record = client.project_config(None).await?;
    // The pinned Rosen configuration has no `ci` table of its own, so the list is read from
    // the text of the file Rosen reports in force. A file Rosen refuses is never that one.
    let dispatchable = match record.source {
        Some(source) => dispatchable(&config_text(&client, &source.to_string()).await?),
        None => Vec::new(),
    };
    Ok(WorkflowPolicy {
        dispatchable,
        mirror: record.config.mirror.remote,
        refused: record.error,
    })
}

async fn config_text(client: &Client, commit: &str) -> Result<String, RosenError> {
    let request = ReadRequest {
        at: Some(commit.to_owned()),
        path: CONFIG_PATH.to_owned(),
    };
    match client.read_file(request).await {
        Ok(ReadAnswer::File { data, .. }) => Ok(String::from_utf8_lossy(&data).into_owned()),
        Ok(ReadAnswer::Directory { .. }) => Ok(String::new()),
        Err(ClientError::Server(error)) if error.code == ErrorCode::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

/// The names under `[ci] dispatchable`. Anything that is not a list of strings allows
/// nothing, so a mistyped list never widens what agents may run.
fn dispatchable(config: &str) -> Vec<String> {
    let Ok(document) = config.parse::<DocumentMut>() else {
        return Vec::new();
    };
    let Some(names) = document
        .get("ci")
        .and_then(|ci| ci.get("dispatchable"))
        .and_then(Item::as_array)
    else {
        return Vec::new();
    };
    let listed: Option<Vec<String>> = names
        .iter()
        .map(|name| name.as_str().map(str::to_owned))
        .collect();
    listed.unwrap_or_default()
}
