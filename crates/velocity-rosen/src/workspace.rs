//! Agent workspaces: a Rosen change and its workspace for each agent, kept stored on the
//! server by a write-through watcher while the agent works (Rosen R2, R3, R4).

use std::collections::{BTreeMap, BTreeSet};
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

pub(crate) async fn session(server: &ProjectServer, agent: &str) -> Result<Session, RosenError> {
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

/// Gives the existing change `change` a workspace at `root` again, at its current head: a
/// change Rosen reverted and reopened lost its workspace when it landed (R4, R12). A folder
/// that is still a workspace is left as it is. Folders a guarded deletion left behind,
/// empty of files, are cleared first; a folder that holds files is never replaced.
pub async fn reopen_workspace(
    server: &ProjectServer,
    agent: &str,
    change: &str,
    root: &Path,
) -> Result<(), RosenError> {
    if rosen_client::snapshot::WorkspaceMarker::path(root).is_file() {
        return Ok(());
    }
    if root.exists() {
        remove_empty_folders(root).map_err(|source| RosenError::Io {
            action: "replace",
            path: root.to_path_buf(),
            source,
        })?;
    }
    session(server, agent)
        .await?
        .open_workspace(change, root, rosen_client::Sparse::all())
        .await?;
    Ok(())
}

/// Removes `folder` when nothing but folders is under it; fails at the first file.
fn remove_empty_folders(folder: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            return Err(std::io::Error::other("the folder still holds files"));
        }
        remove_empty_folders(&entry.path())?;
    }
    std::fs::remove_dir(folder)
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
    /// Workspaces a rebase is being written into: no watcher starts there meanwhile.
    held: Mutex<BTreeSet<PathBuf>>,
    /// Workspaces whose watcher is starting and not yet in `running`, where a rebase's stop
    /// would miss it. Each side marks itself before it checks the other, so of a start and
    /// a rebase that race, at least one sees the other and stands back.
    starting: Mutex<BTreeSet<PathBuf>>,
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
        if !mark(&self.starting, root) {
            return Ok(());
        }
        let started = self.start_marked(server, agent, root).await;
        unmark(&self.starting, root);
        started
    }

    async fn start_marked(
        &self,
        server: &ProjectServer,
        agent: &str,
        root: &Path,
    ) -> Result<(), RosenError> {
        if self.is_watching(root) || self.is_held(root) {
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
        if self.is_held(root) {
            // A rebase began while this watcher started: it must not run under the write.
            watch.stop().await?;
            return Ok(());
        }
        if let Ok(mut running) = self.running.lock() {
            running.retain(|_, watch| !watch.is_finished());
            running.entry(root.to_path_buf()).or_insert(watch);
        }
        Ok(())
    }

    fn is_held(&self, root: &Path) -> bool {
        self.held.lock().is_ok_and(|held| held.contains(root))
    }

    /// Writes the server's rebase of the change into its workspace at `root` (Rosen R2), if
    /// one is waiting: stops the watcher, stores a final snapshot of the workspace, writes
    /// the change's rebased head and starts the watcher again. Call it at the agent's step
    /// boundary, when nothing writes there. Rosen refuses, leaving the workspace as it was,
    /// while an edit there is not stored. Returns the head now in the workspace, or `None`
    /// when no rebase waited or another call is already writing one there.
    pub async fn apply_rebase(
        &self,
        server: &ProjectServer,
        agent: &str,
        root: &Path,
    ) -> Result<Option<String>, RosenError> {
        if !mark(&self.held, root) {
            return Ok(None);
        }
        // A watcher still starting would hold the workspace unseen; the rebase waits on the
        // server for the next check.
        if self
            .starting
            .lock()
            .is_ok_and(|starting| starting.contains(root))
        {
            unmark(&self.held, root);
            return Ok(None);
        }
        let applied = self.write_rebase(server, agent, root).await;
        unmark(&self.held, root);
        let restarted = self.start(server, agent, root).await;
        let head = applied?;
        restarted?;
        Ok(head)
    }

    async fn write_rebase(
        &self,
        server: &ProjectServer,
        agent: &str,
        root: &Path,
    ) -> Result<Option<String>, RosenError> {
        let session = session(server, agent).await?;
        if !rebase_waiting(&session, root).await? {
            return Ok(None);
        }
        self.stop(root).await;
        session.snapshot(root, None).await?;
        Ok(Some(session.apply_rebase(root).await?.to_string()))
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

/// Adds `root` to `set`; false when it was there already.
fn mark(set: &Mutex<BTreeSet<PathBuf>>, root: &Path) -> bool {
    set.lock()
        .is_ok_and(|mut set| set.insert(root.to_path_buf()))
}

fn unmark(set: &Mutex<BTreeSet<PathBuf>>, root: &Path) {
    if let Ok(mut set) = set.lock() {
        set.remove(root);
    }
}

/// Whether Rosen holds a head for the workspace's open change that the workspace doesn't:
/// the server rebased it (R2), and that lasts until the rebase is applied, across restarts.
/// The workspace's marker names the tree of its last stored snapshot, which is the change's
/// head unless the server moved it.
async fn rebase_waiting(session: &Session, root: &Path) -> Result<bool, RosenError> {
    let marker =
        rosen_client::snapshot::WorkspaceMarker::read(root).map_err(|source| RosenError::Io {
            action: "read the Rosen marker of",
            path: root.to_path_buf(),
            source,
        })?;
    let record = session
        .client()
        .change_show(&marker.change.to_string())
        .await?
        .change;
    let open = matches!(
        record.state,
        rosen_client::ChangeState::Open | rosen_client::ChangeState::Ready
    );
    Ok(open && record.head != marker.tree)
}
