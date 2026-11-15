//! Continue a task on an unlanded dependency without discarding its original change.
use std::path::Path;

use rosen_client::protocol::{DiffRequest, ReadAnswer, ReadRequest};
use rosen_client::{ChangeState, Sparse};

use crate::{AgentWorkspace, ProjectServer, RosenError};

/// The caller stops the old watcher first. The old change stays intact until the caller
/// saves the replacement on its thread; even a failed copy never loses the task's work.
pub async fn stack_workspace(
    server: &ProjectServer,
    agent: &str,
    original: &Path,
    parent: &str,
    root: &Path,
) -> Result<AgentWorkspace, RosenError> {
    let source_session = crate::workspace::session(server, &crate::agent_name(original)).await?;
    source_session.snapshot(original, None).await?;
    let client = source_session.client();
    let marker = rosen_client::snapshot::WorkspaceMarker::read(original)
        .map_err(rosen_client::Error::from)?;
    let source = marker.change.to_string();
    let record = client.change_show(&source).await?;
    if record.change.state != ChangeState::Open {
        return Err(invalid("Only unfinished work can be stacked"));
    }
    let session = crate::workspace::session(server, agent).await?;
    if let Some(directory) = root.parent() {
        std::fs::create_dir_all(directory).map_err(rosen_client::Error::from)?;
    }
    let child = session.new_change(Some(parent)).await?;
    let change = child.change.id.to_string();
    let copied = copy_edits(session.client(), &source, &change).await;
    if let Err(error) = copied {
        let _ = session.abandon(&change).await;
        return Err(error);
    }
    if let Err(error) = session.open_workspace(&change, root, Sparse::all()).await {
        let _ = session.abandon(&change).await;
        return Err(error.into());
    }
    Ok(AgentWorkspace::new(
        change,
        root.to_path_buf(),
        "stacked",
        false,
    ))
}

async fn copy_edits(
    client: &rosen_client::Client,
    source: &str,
    child: &str,
) -> Result<(), RosenError> {
    let edits = client.workspace_diff(source).await?;
    let parent_edits = client
        .diff(DiffRequest {
            from: Some(edits.from.to_string()),
            to: Some(child.to_owned()),
            ..DiffRequest::default()
        })
        .await?;
    // Do not overwrite a dependency's edits with a whole-file copy. Overlapping work
    // keeps its original change and waits for the parent, then uses Rosen's rebase.
    for file in &edits.files {
        if parent_edits
            .files
            .iter()
            .any(|other| other.path == file.path)
        {
            return Err(invalid(&format!(
                "Stack overlaps {}; original work kept",
                file.path
            )));
        }
        if file
            .new
            .as_ref()
            .is_some_and(|version| version.mode != "100644")
        {
            return Err(invalid("Stack requires a regular file; original work kept"));
        }
    }
    for file in edits.files {
        let bytes = if file.new.is_some() {
            match client
                .read_file(ReadRequest {
                    at: Some(edits.to.to_string()),
                    path: file.path.clone(),
                })
                .await?
            {
                ReadAnswer::File { data, .. } => Some(data),
                _ => return Err(invalid("Stack source is no longer a file")),
            }
        } else {
            None
        };
        client.workspace_write(child, &file.path, bytes).await?;
    }
    Ok(())
}

fn invalid(message: &str) -> RosenError {
    RosenError::Client(rosen_client::Error::InvalidArgument(message.into()))
}
