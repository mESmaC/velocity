//! Agent workspaces: a Rosen change and its workspace for each agent, kept stored on the
//! server by a write-through watcher while the agent works (Rosen R2, R3, R4).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rosen_client::{Session, WorkspaceSpec, WorkspaceWatch};

use crate::{ProjectServer, RosenError};

/// How long a new watcher has to fail before it counts as started.
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// A new change and the workspace that holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentWorkspace {
    pub change: String,
    pub root: PathBuf,
    /// How Rosen made it: `warm-pool`, `block-clone` or `plain-copy`.
    pub method: &'static str,
    /// Whether the project's prepare commands had already run in it.
    pub prepared: bool,
}

async fn session(server: &ProjectServer, agent: &str) -> Result<Session, RosenError> {
    let agent = rosen_client::identity::parse_agent(agent)
        .map_err(|_| RosenError::AgentName(agent.to_owned()))?;
    Ok(Session::builder()
        .endpoint(server.endpoint().clone())
        .agent(agent)
        // Velocity starts the server itself, with the project's own data folder.
        .start_daemon(false)
        .open()
        .await?)
}

/// Creates a change owned by `agent` and its workspace at `root`, which must not exist
/// yet, stacked on `on` when given. A ready workspace is taken from the warm pool when
/// one is free. If the workspace fails, the change it made is abandoned.
pub async fn create_workspace(
    server: &ProjectServer,
    agent: &str,
    root: &Path,
    on: Option<&str>,
) -> Result<AgentWorkspace, RosenError> {
    let session = session(server, agent).await?;
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent).map_err(|source| RosenError::Io {
            action: "create",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut spec = WorkspaceSpec::new(root);
    if let Some(change) = on {
        spec = spec.on(change);
    }
    match session.create_workspace(spec).await {
        Ok(created) => Ok(AgentWorkspace {
            change: created.change.change.id.to_string(),
            root: created.root,
            method: created.report.method.as_str(),
            prepared: created.report.prepared,
        }),
        Err(rosen_client::Error::WorkspaceNotCreated { change, source }) => {
            let _ = session.abandon(&change.to_string()).await;
            Err(RosenError::Client(*source))
        }
        Err(error) => Err(error.into()),
    }
}

/// Abandons `change`. Rosen then deletes its workspace under the deletion guard, which
/// keeps it while any edit there is not stored on the server (R4).
pub async fn abandon_change(
    server: &ProjectServer,
    agent: &str,
    change: &str,
) -> Result<(), RosenError> {
    session(server, agent).await?.abandon(change).await?;
    Ok(())
}

/// The write-through watchers Velocity runs, one per live agent workspace. A watcher
/// keeps journalling while the server is down and uploads in order once it is back.
#[derive(Default)]
pub struct Watches {
    running: Mutex<BTreeMap<PathBuf, WorkspaceWatch>>,
}

impl Watches {
    /// Starts watching `root`, unless a watcher already runs there. Call from inside the
    /// async runtime: the watcher is a task on it.
    pub async fn start(
        &self,
        server: &ProjectServer,
        agent: &str,
        root: &Path,
    ) -> Result<(), RosenError> {
        if self.is_watching(root) {
            return Ok(());
        }
        let mut watch = session(server, agent).await?.watch(root);
        // A watcher that can't open its workspace or its file watch ends at once. One that
        // runs reports its first snapshot of the whole workspace, or a failed attempt it
        // will retry. Waiting briefly for either tells the two apart; a large workspace
        // whose first snapshot is still running counts as started.
        if let Ok(None) = tokio::time::timeout(STARTUP_GRACE, watch.next_report()).await {
            watch.stop().await?;
            return Err(RosenError::WatchEnded(root.to_path_buf()));
        }
        if let Ok(mut running) = self.running.lock() {
            running.retain(|_, watch| !watch.is_finished());
            running.entry(root.to_path_buf()).or_insert(watch);
        }
        Ok(())
    }

    /// Whether a watcher runs on `root`. One that ended on its own doesn't count.
    pub fn is_watching(&self, root: &Path) -> bool {
        self.running
            .lock()
            .map(|running| running.get(root).is_some_and(|watch| !watch.is_finished()))
            .unwrap_or(false)
    }

    /// Stops the watcher on `root`, waiting for it to end.
    pub async fn stop(&self, root: &Path) {
        let watch = self
            .running
            .lock()
            .ok()
            .and_then(|mut running| running.remove(root));
        if let Some(watch) = watch {
            let _ = watch.stop().await;
        }
    }
}
