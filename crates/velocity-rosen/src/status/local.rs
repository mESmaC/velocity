use std::path::{Path, PathBuf};

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use rosen_client::run_queue::{QUEUE_ENV, RunQueue};
use tokio::sync::mpsc;

use super::LocalStatus;
use crate::ProjectServer;

pub(super) struct Notifications {
    _watcher: Option<RecommendedWatcher>,
    updates: mpsc::Receiver<()>,
}

impl Notifications {
    pub fn watch(server: &ProjectServer) -> Self {
        let (send, updates) = mpsc::channel(1);
        let pool = pool_root(server);
        let queue = queue_root();
        let database = server.data_dir().join("metadata.sqlite");
        let journal = database.with_file_name("metadata.sqlite-wal");
        let affected_pool = pool.clone();
        let affected_queue = queue.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            if let Ok(event) = event
                && !event.kind.is_access()
                && event.paths.iter().any(|path| {
                    *path == database
                        || *path == journal
                        || pool_changed(path, &affected_pool)
                        || affected_queue
                            .as_ref()
                            .is_some_and(|queue| path.starts_with(queue))
                })
            {
                let _ = send.try_send(());
            }
        });
        let _watcher = watcher.ok().map(|mut watcher| {
            watch_from(&mut watcher, server.data_dir(), RecursiveMode::NonRecursive);
            if let Some(parent) = pool.parent() {
                watch_from(&mut watcher, parent, RecursiveMode::Recursive);
            }
            if let Some(parent) = queue.as_deref().and_then(Path::parent) {
                watch_from(&mut watcher, parent, RecursiveMode::Recursive);
            }
            watcher
        });
        Self { _watcher, updates }
    }

    pub async fn next(&mut self) {
        if self.updates.recv().await.is_none() {
            std::future::pending::<()>().await;
        }
    }
}

fn watch_from(watcher: &mut RecommendedWatcher, path: &Path, mode: RecursiveMode) {
    if let Some(existing) = path.ancestors().find(|folder| folder.is_dir()) {
        let mode = if existing == path {
            mode
        } else {
            RecursiveMode::Recursive
        };
        if let Err(error) = watcher.watch(existing, mode) {
            eprintln!(
                "Rosen status couldn't watch {}: {error}",
                existing.display()
            );
        }
    }
}

fn pool_changed(path: &Path, pool: &Path) -> bool {
    path.strip_prefix(pool).is_ok_and(|relative| {
        relative.components().count() <= 1 || relative.ends_with(".rosen/stat-cache.json")
    })
}

pub(super) fn read(server: &ProjectServer) -> LocalStatus {
    let queue = RunQueue::machine()
        .and_then(|queue| queue.line())
        .unwrap_or_default();
    let mut holders = Vec::new();
    let mut waiting = 0;
    for entry in queue {
        if entry.running {
            // Standard Rosen notes end with the workspace. The command itself can
            // contain arbitrary arguments, so show the workspace and queue class.
            let workspace = entry
                .note
                .rsplit_once(" in ")
                .and_then(|(_, path)| Path::new(path.trim()).file_name())
                .and_then(|name| name.to_str());
            let class = entry.class.name();
            holders.push(
                workspace.map_or_else(|| class.to_owned(), |name| format!("{name} · {class}")),
            );
        } else {
            waiting += 1;
        }
    }
    LocalStatus {
        holders,
        waiting,
        ready: ready(server),
        target: 0,
    }
}

fn ready(server: &ProjectServer) -> u32 {
    let folder = pool_root(server);
    let count = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name.split_once('-').is_some_and(|(pid, slot)| {
                    pid.parse::<u32>().is_ok() && slot.parse::<u64>().is_ok()
                })
            }) && entry.path().join(".rosen/stat-cache.json").is_file()
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

// The pinned daemon keeps the pool beside the workspace volume; the hash
// distinguishes projects that share that directory.
fn pool_root(server: &ProjectServer) -> PathBuf {
    let Some(folder) = server.warm_pool_dir() else {
        return server.data_dir().join("warm-pool");
    };
    let data =
        std::path::absolute(server.data_dir()).unwrap_or_else(|_| server.data_dir().to_owned());
    let owner = rosen_core::ObjectId::hash_object("warm-pool", data.as_os_str().as_encoded_bytes());
    folder.join(format!("rosen-warm-pool-{}", &owner.to_hex()[..16]))
}

// Match RunQueue::machine's location for its filesystem notifications.
fn queue_root() -> Option<PathBuf> {
    if let Some(folder) = std::env::var_os(QUEUE_ENV) {
        return Some(folder.into());
    }
    #[cfg(windows)]
    let parent = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    #[cfg(not(windows))]
    let parent = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;
    Some(parent.join("rosen").join("run-queue"))
}
