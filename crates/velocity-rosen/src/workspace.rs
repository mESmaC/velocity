//! Agent workspaces: a Rosen change and its workspace for each agent, kept stored on the
//! server by a write-through watcher while the agent works (Rosen R2, R3, R4).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rosen_client::{Session, WorkspaceSpec, WorkspaceWatch};

use crate::connection::Scoped;
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

impl AgentWorkspace {
    pub fn new(change: String, root: PathBuf, method: &'static str, prepared: bool) -> Self {
        Self {
            change,
            root,
            method,
            prepared,
        }
    }
}

pub(crate) async fn session(server: &ProjectServer, agent: &str) -> Result<Session, RosenError> {
    let connection = crate::connection::requests(server.endpoint()).await?;
    session_on(&connection, server, agent)
}

fn session_on(
    connection: &crate::connection::Connection,
    server: &ProjectServer,
    agent: &str,
) -> Result<Session, RosenError> {
    let agent = rosen_client::identity::parse_agent(agent)
        .map_err(|_| RosenError::AgentName(agent.to_owned()))?;
    Ok(Session::from_client(
        connection.client(),
        server.endpoint().clone(),
        agent,
    ))
}

/// Creates a change owned by `agent`, stacked on `on` when given, with no workspace yet.
/// The caller saves the change before any file is written, so a slow or failed workspace
/// never leaves a change that no thread knows (#514).
pub async fn create_change(
    server: &ProjectServer,
    agent: &str,
    on: Option<&str>,
) -> Result<String, RosenError> {
    let record = session(server, agent).await?.new_change(on).await?;
    Ok(record.change.id.to_string())
}

/// Gives `change` its workspace at `root`, as [`reopen_workspace`] does, without waiting on
/// one connection for ever (#514). Reuses an existing folder only when its
/// `.rosen/workspace.json` marker names `change`; a folder carrying another change's marker
/// is left intact and refused (#659). After `limit` with no workspace, `fell_back` is called
/// and a second attempt starts on a connection of its own, which is what
/// `rosen change new --workspace` has: a request held up on Velocity's shared connection
/// cannot delay it. Both attempts then run for `limit` more and the first workspace wins.
/// Rosen publishes a workspace by renaming it into place, so the attempt that loses finds
/// the folder taken and writes nothing into it.
pub async fn open_workspace_within(
    server: &ProjectServer,
    agent: &str,
    change: &str,
    root: &Path,
    limit: std::time::Duration,
    fell_back: impl FnOnce(),
) -> Result<AgentWorkspace, RosenError> {
    if prepare_root(root, change)? {
        let root = root.to_path_buf();
        return Ok(AgentWorkspace::new(
            change.to_owned(),
            root,
            "reopened",
            false,
        ));
    }
    let shared = open_on(server, agent, change, root, false);
    tokio::pin!(shared);
    if let Ok(opened) = tokio::time::timeout(limit, &mut shared).await {
        return opened;
    }
    fell_back();
    let direct = open_on(server, agent, change, root, true);
    tokio::pin!(direct);
    let deadline = tokio::time::sleep(limit);
    tokio::pin!(deadline);
    let (mut shared_failed, mut direct_failed) = (false, None);
    loop {
        tokio::select! {
            opened = &mut shared, if !shared_failed => match opened {
                Ok(workspace) => return Ok(workspace),
                // The direct attempt may have taken the folder first; its result decides.
                Err(_) => shared_failed = true,
            },
            opened = &mut direct, if direct_failed.is_none() => match opened {
                Ok(workspace) => return Ok(workspace),
                Err(error) => direct_failed = Some(error),
            },
            () = &mut deadline => {
                return Err(RosenError::WorkspaceWait {
                    root: root.to_path_buf(),
                    seconds: limit.as_secs().saturating_mul(2),
                });
            }
        }
        if shared_failed && let Some(error) = direct_failed.take() {
            return Err(error);
        }
    }
}

/// One attempt at `change`'s workspace: on the connection Velocity's requests share, or
/// on one opened for this attempt alone and closed with it.
async fn open_on(
    server: &ProjectServer,
    agent: &str,
    change: &str,
    root: &Path,
    dedicated: bool,
) -> Result<AgentWorkspace, RosenError> {
    let sparse = rosen_client::Sparse::all();
    let report = if dedicated {
        let connection = crate::connection::Connection::dedicated(server.endpoint()).await?;
        session_on(&connection, server, agent)?
            .open_workspace(change, root, sparse)
            .await?
    } else {
        session(server, agent)
            .await?
            .open_workspace(change, root, sparse)
            .await?
    };
    Ok(AgentWorkspace {
        change: change.to_owned(),
        root: root.to_path_buf(),
        method: report.method.as_str(),
        prepared: report.prepared,
    })
}

/// Whether `root` currently holds an initialized Rosen workspace on disk.
pub fn has_workspace(root: &Path) -> bool {
    root.is_dir() && rosen_client::snapshot::WorkspaceMarker::path(root).is_file()
}

/// Verifies that an existing Rosen workspace at `root` belongs to `expected` by reading
/// `.rosen/workspace.json`, without touching anything on disk (#659).
pub fn verify_workspace_change(root: &Path, expected: &str) -> Result<(), RosenError> {
    let marker =
        rosen_client::snapshot::WorkspaceMarker::read(root).map_err(|source| RosenError::Io {
            action: "read the Rosen marker of",
            path: root.to_path_buf(),
            source,
        })?;
    let existing = marker.change.to_string();
    if existing != expected {
        return Err(RosenError::WorkspaceChangeMismatch {
            root: root.to_path_buf(),
            existing,
            expected: expected.to_owned(),
        });
    }
    Ok(())
}

/// Refuses to launch a brand-new change at `root` when a Rosen workspace already lives
/// there, preserving the existing folder untouched (#659).
pub fn ensure_no_existing_workspace(root: &Path) -> Result<(), RosenError> {
    if !has_workspace(root) {
        return Ok(());
    }
    let marker =
        rosen_client::snapshot::WorkspaceMarker::read(root).map_err(|source| RosenError::Io {
            action: "read the Rosen marker of",
            path: root.to_path_buf(),
            source,
        })?;
    Err(RosenError::WorkspaceOccupied {
        root: root.to_path_buf(),
        existing: marker.change.to_string(),
    })
}

/// Clears an existing directory that holds only empty subdirectories and no files,
/// matching [`prepare_root`] when recreating a workspace after guarded deletion.
pub(crate) fn clear_empty_root(root: &Path) -> Result<(), RosenError> {
    if !root.exists() {
        return Ok(());
    }
    remove_empty_folders(root).map_err(|source| RosenError::Io {
        action: "replace",
        path: root.to_path_buf(),
        source,
    })
}

/// Readies `root` for `change`'s workspace. Returns `true` only when `root` already holds
/// a Rosen workspace whose `.rosen/workspace.json` marker names `change`, leaving it intact
/// (#659). A workspace belonging to another change, or with an unreadable marker, is refused
/// without modifying the folder. Folders a guarded deletion left behind, empty of files, are
/// cleared; a folder that holds files is never replaced.
fn prepare_root(root: &Path, change: &str) -> Result<bool, RosenError> {
    if has_workspace(root) {
        verify_workspace_change(root, change)?;
        return Ok(true);
    }
    clear_empty_root(root)?;
    if let Some(parent) = root.parent() {
        std::fs::create_dir_all(parent).map_err(|source| RosenError::Io {
            action: "create",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    Ok(false)
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
    match create_workspace_retaining_change(server, agent, root, on).await {
        Err(RosenError::WorkspaceNotCreated { change, source }) => {
            let _ = abandon_change(server, agent, &change).await;
            Err(RosenError::Client(*source))
        }
        result => result,
    }
}

/// Like `create_workspace`, but retains an allocated change on failure so a thread can
/// save its identity and retry `reopen_workspace` without creating a second change.
pub async fn create_workspace_retaining_change(
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
            Err(RosenError::WorkspaceNotCreated {
                change: change.to_string(),
                source,
            })
        }
        Err(error) => Err(error.into()),
    }
}

/// Gives the existing change `change` a workspace at `root` again, at its current head: a
/// change Rosen reverted and reopened lost its workspace when it landed (R4, R12). A folder
/// that is already a workspace for `change` is left as it is; a workspace belonging to
/// another change is refused and kept intact (#659). Folders a guarded deletion left behind,
/// empty of files, are cleared first; a folder that holds files is never replaced.
pub async fn reopen_workspace(
    server: &ProjectServer,
    agent: &str,
    change: &str,
    root: &Path,
) -> Result<(), RosenError> {
    if prepare_root(root, change)? {
        return Ok(());
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
    running: Mutex<BTreeMap<PathBuf, Scoped<WorkspaceWatch>>>,
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
        let session = session(server, agent).await?;
        let watched_root = root.to_path_buf();
        let mut watch =
            Scoped::start(move || async move { Ok(session.watch(&watched_root)) }).await?;
        // A watcher that can't open its workspace or its file watch ends at once. One that
        // runs reports its first snapshot of the whole workspace, or a failed attempt it
        // will retry. Waiting briefly for either tells the two apart; a large workspace
        // whose first snapshot is still running counts as started.
        if let Ok(None) = tokio::time::timeout(STARTUP_GRACE, watch.next_report()).await {
            stop_watch(watch).await?;
            return Err(RosenError::WatchEnded(root.to_path_buf()));
        }
        if self.is_held(root) {
            // A rebase began while this watcher started: it must not run under the write.
            stop_watch(watch).await?;
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
    /// one is waiting: stops the watcher, writes
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
        // Do not resend the old workspace before exposing the rebase. Open-conflict
        // files take a snapshot verbatim, so that would replace their marked head with
        // the unapplied version. Unstored edits refuse application and the restarted
        // watcher stores them before the next boundary retries.
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
            let _ = stop_watch(watch).await;
        }
    }
}

async fn stop_watch(watch: Scoped<WorkspaceWatch>) -> Result<(), rosen_client::Error> {
    let Scoped { value, stop } = watch;
    let result = value.stop().await;
    drop(stop);
    result
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
pub async fn rebase_pending(
    server: &ProjectServer,
    agent: &str,
    root: &Path,
) -> Result<bool, RosenError> {
    rebase_waiting(&session(server, agent).await?, root).await
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use rosen_client::snapshot::WorkspaceMarker;
    use rosen_core::{ChangeId, ObjectId};

    fn write_marker(root: &Path, byte: u8) -> Result<String, Box<dyn std::error::Error>> {
        let change = ChangeId::from_bytes([byte; 16]);
        let tree: ObjectId = "a".repeat(40).parse()?;
        WorkspaceMarker::new(change, tree, Vec::new(), Vec::new()).write(root)?;
        Ok(change.to_string())
    }

    // Risk (#659): thread launch reuse previously trusted `.rosen/workspace.json` existence alone,
    // adopting a foreign change's folder and running per-agent setup in it.
    #[tokio::test]
    async fn launch_openers_require_matching_marker_and_preserve_foreign_folder()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("workspace-a");
        let existing_change = write_marker(&root, 0x11)?;
        let requested_change = ChangeId::from_bytes([0x22; 16]).to_string();
        let sentinel = root.join("work.txt");
        std::fs::write(&sentinel, "do not touch")?;
        let server = ProjectServer::new(dir.path(), 1);

        let opened = open_workspace_within(
            &server,
            "agent",
            &existing_change,
            &root,
            std::time::Duration::from_secs(1),
            || {},
        )
        .await?;
        assert_eq!(
            opened,
            AgentWorkspace::new(existing_change.clone(), root.clone(), "reopened", false)
        );
        reopen_workspace(&server, "agent", &existing_change, &root).await?;

        let mismatch = open_workspace_within(
            &server,
            "agent",
            &requested_change,
            &root,
            std::time::Duration::from_secs(1),
            || {},
        )
        .await;
        assert!(matches!(
            mismatch,
            Err(RosenError::WorkspaceChangeMismatch {
                ref existing,
                ref expected,
                ..
            }) if existing == &existing_change && expected == &requested_change
        ));
        assert!(matches!(
            reopen_workspace(&server, "agent", &requested_change, &root).await,
            Err(RosenError::WorkspaceChangeMismatch { .. })
        ));
        assert!(matches!(
            ensure_no_existing_workspace(&root),
            Err(RosenError::WorkspaceOccupied { ref existing, .. }) if existing == &existing_change
        ));
        assert_eq!(std::fs::read_to_string(&sentinel)?, "do not touch");
        assert_eq!(
            WorkspaceMarker::read(&root)?.change.to_string(),
            existing_change
        );
        Ok(())
    }

    // Risk (#659): an unreadable marker or non-empty folder must never be wiped or adopted.
    #[test]
    fn prepare_root_preserves_corrupt_marker_and_clears_only_empty_dirs()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let corrupt = dir.path().join("corrupt");
        std::fs::create_dir_all(corrupt.join(".rosen"))?;
        std::fs::write(WorkspaceMarker::path(&corrupt), "{not-json")?;
        let sentinel = corrupt.join("keep.txt");
        std::fs::write(&sentinel, "kept")?;

        let requested = ChangeId::from_bytes([0x33; 16]).to_string();
        assert!(matches!(
            prepare_root(&corrupt, &requested),
            Err(RosenError::Io { .. })
        ));
        assert!(matches!(
            ensure_no_existing_workspace(&corrupt),
            Err(RosenError::Io { .. })
        ));
        assert_eq!(std::fs::read_to_string(&sentinel)?, "kept");

        let empty = dir.path().join("empty");
        std::fs::create_dir_all(empty.join("nested"))?;
        assert!(!prepare_root(&empty, &requested)?);
        assert!(!empty.exists());
        assert!(ensure_no_existing_workspace(&empty).is_ok());

        let non_empty = dir.path().join("non-empty");
        std::fs::create_dir_all(&non_empty)?;
        let leftover = non_empty.join("file.txt");
        std::fs::write(&leftover, "present")?;
        assert!(matches!(
            prepare_root(&non_empty, &requested),
            Err(RosenError::Io { .. })
        ));
        assert_eq!(std::fs::read_to_string(&leftover)?, "present");
        Ok(())
    }
}
