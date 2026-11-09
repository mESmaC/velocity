//! Native drive/size measurements and cached worktree inventory (SPEC.md F5).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use sysinfo::{DiskKind, Disks};
use thiserror::Error;
use velocity_core::worktree_settings::deleted_row_window_start;
use velocity_core::worktree_view::{
    self, DriveInfo, WorktreeContext, WorktreeRowTone, WorktreeRowView, WorktreeSettingsView,
    WorktreeSize, path_key,
};

#[derive(Debug, Error)]
pub enum InventoryError {
    #[error("Could not read drive information: {0}")]
    Drives(String),
    #[error("Could not measure '{path}': {source}")]
    Measure {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("The size of '{0}' exceeds the supported measurement range")]
    SizeOverflow(PathBuf),
    #[error("Worktree size measurement stopped")]
    Stopped,
    #[error("Could not access worktree inventory: {0}")]
    State(String),
    #[error("Could not start worktree inventory worker: {0}")]
    Worker(#[source] std::io::Error),
}

pub trait WorktreeSource: Send + Sync + 'static {
    fn context(&self) -> Result<WorktreeContext, InventoryError>;
    fn paths(&self) -> Result<Vec<String>, InventoryError>;
}

#[derive(Default)]
pub struct WorktreeRuntime {
    inventory: Mutex<WorktreeInventory>,
    stopped: AtomicBool,
}

impl WorktreeRuntime {
    pub fn snapshot(
        &self,
        context: &WorktreeContext,
    ) -> Result<WorktreeSettingsView, InventoryError> {
        self.inventory
            .lock()
            .map_err(|error| InventoryError::State(error.to_string()))
            .map(|mut inventory| inventory.snapshot(context))
    }

    pub fn location_changed(&self) -> Result<(), InventoryError> {
        self.inventory
            .lock()
            .map_err(|error| InventoryError::State(error.to_string()))
            .map(|mut inventory| inventory.mark_location_changed())
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    pub fn start(
        self: &Arc<Self>,
        source: Arc<dyn WorktreeSource>,
        publish: Arc<dyn Fn(WorktreeSettingsView) + Send + Sync>,
    ) -> Result<(), InventoryError> {
        let runtime = Arc::clone(self);
        let state_source = Arc::clone(&source);
        std::thread::Builder::new()
            .name("velocity-worktree-state".into())
            .spawn(move || {
                let mut published = None;
                while !runtime.stopped.load(Ordering::Relaxed) {
                    match state_source
                        .context()
                        .and_then(|context| runtime.snapshot(&context))
                    {
                        Ok(view) if published != Some(view.revision) => {
                            published = Some(view.revision);
                            publish(view);
                        }
                        Ok(_) => {}
                        Err(error) => eprintln!("{error}"),
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            })
            .map_err(InventoryError::Worker)?;
        let runtime = Arc::clone(self);
        if let Err(error) = std::thread::Builder::new()
            .name("velocity-worktree-metrics".into())
            .spawn(move || runtime.measure_loop(source))
        {
            self.stop();
            return Err(InventoryError::Worker(error));
        }
        Ok(())
    }

    fn measure_loop(&self, source: Arc<dyn WorktreeSource>) {
        let mut measured: BTreeMap<String, Instant> = BTreeMap::new();
        let mut drives_at = None;
        while !self.stopped.load(Ordering::Relaxed) {
            if drives_at.is_none_or(|time: Instant| time.elapsed() >= Duration::from_secs(10)) {
                let drives = read_drives();
                if let Ok(mut inventory) = self.inventory.lock() {
                    inventory.update_drives(drives);
                }
                drives_at = Some(Instant::now());
            }
            match source.paths() {
                Ok(paths) => {
                    measured
                        .retain(|path, _| paths.iter().any(|current| path_key(current) == *path));
                    for path in paths {
                        if self.stopped.load(Ordering::Relaxed) {
                            break;
                        }
                        let key = path_key(&path);
                        if measured
                            .get(&key)
                            .is_some_and(|time| time.elapsed() < Duration::from_secs(30))
                        {
                            continue;
                        }
                        let result = measure_worktree(Path::new(&path), &self.stopped);
                        if let Ok(mut inventory) = self.inventory.lock() {
                            inventory.update_size(path, result);
                        }
                        measured.insert(key, Instant::now());
                    }
                }
                Err(error) => eprintln!("{error}"),
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

pub fn read_drives() -> Result<Vec<DriveInfo>, InventoryError> {
    let disks = Disks::new_with_refreshed_list()
        .map_err(|error| InventoryError::Drives(error.to_string()))?;
    Ok(disks
        .iter()
        .map(|disk| DriveInfo {
            root: disk.mount_point().to_string_lossy().into_owned(),
            name: disk.name().to_string_lossy().into_owned(),
            kind: format!(
                "{}{}",
                if disk.is_removable() {
                    "Removable "
                } else {
                    ""
                },
                match disk.kind() {
                    DiskKind::SSD => "SSD",
                    DiskKind::HDD => "HDD",
                    _ => "Disk",
                }
            ),
            total_bytes: disk.total_space(),
            free_bytes: disk.available_space(),
            slow: matches!(disk.kind(), DiskKind::HDD),
        })
        .collect())
}

fn measure_error(path: &Path, source: std::io::Error) -> InventoryError {
    InventoryError::Measure {
        path: path.to_path_buf(),
        source,
    }
}

fn linked_directory(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Directory reparse points include junctions: following them would count
        // shared caches repeatedly and can create cycles outside the worktree.
        metadata.is_dir() && metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

/// Apparent file bytes, excluding linked directories. Canonicalizing the root
/// gives Windows an extended-length path; descendants keep that prefix.
pub fn measure_worktree(path: &Path, stopped: &AtomicBool) -> Result<u64, InventoryError> {
    let root = fs::canonicalize(path).map_err(|error| measure_error(path, error))?;
    let mut directories = vec![root];
    let mut bytes = 0_u64;
    while let Some(directory) = directories.pop() {
        if stopped.load(Ordering::Relaxed) {
            return Err(InventoryError::Stopped);
        }
        for entry in fs::read_dir(&directory).map_err(|error| measure_error(&directory, error))? {
            if stopped.load(Ordering::Relaxed) {
                return Err(InventoryError::Stopped);
            }
            let entry = entry.map_err(|error| measure_error(&directory, error))?;
            let entry_path = entry.path();
            let metadata = fs::symlink_metadata(&entry_path)
                .map_err(|error| measure_error(&entry_path, error))?;
            if linked_directory(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                directories.push(entry_path);
            } else {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or_else(|| InventoryError::SizeOverflow(path.to_path_buf()))?;
            }
        }
    }
    Ok(bytes)
}

#[derive(Default)]
pub struct WorktreeInventory {
    drives: Vec<DriveInfo>,
    sizes: BTreeMap<String, WorktreeSize>,
    previous: BTreeMap<Option<u32>, Vec<WorktreeRowView>>,
    deleted: BTreeMap<Option<u32>, Vec<(Instant, WorktreeRowView)>>,
    latest: Option<WorktreeSettingsView>,
    context_revision: u32,
    location_changed: bool,
    drive_error: String,
}

impl WorktreeInventory {
    pub fn update_drives(&mut self, result: Result<Vec<DriveInfo>, InventoryError>) {
        match result {
            Ok(drives) => {
                self.drives = drives;
                self.drive_error.clear();
            }
            Err(error) => self.drive_error = error.to_string(),
        }
    }

    pub fn update_size(&mut self, path: String, result: Result<u64, InventoryError>) {
        let (bytes, error) = match result {
            Ok(bytes) => (Some(bytes), None),
            Err(error) => (None, Some(error.to_string())),
        };
        self.sizes
            .insert(path_key(&path), WorktreeSize { path, bytes, error });
    }

    pub fn mark_location_changed(&mut self) {
        self.location_changed = true;
    }

    pub fn snapshot(&mut self, context: &WorktreeContext) -> WorktreeSettingsView {
        if context.revision < self.context_revision
            && let Some(view) = &self.latest
        {
            return view.clone();
        }
        self.context_revision = context.revision;
        let current = worktree_view::settings_view(
            context,
            &self.drives,
            &self.sizes,
            &[],
            self.location_changed,
        );
        self.track_deleted(
            context.project_identifier,
            &current.rows,
            &context.merged_owners,
        );
        let deleted: Vec<_> = self
            .deleted
            .get(&context.project_identifier)
            .into_iter()
            .flatten()
            .map(|(_, row)| row.clone())
            .collect();
        let mut view = worktree_view::settings_view(
            context,
            &self.drives,
            &self.sizes,
            &deleted,
            self.location_changed,
        );
        if !self.drive_error.is_empty() {
            view.free_space = self.drive_error.clone();
        }
        if let Some(previous) = &self.latest {
            view.revision = previous.revision;
            if view != *previous {
                view.revision = view.revision.wrapping_add(1);
            }
        }
        self.latest = Some(view.clone());
        view
    }

    fn track_deleted(&mut self, project: Option<u32>, rows: &[WorktreeRowView], merged: &[u32]) {
        let now = Instant::now();
        let recent = self.deleted.entry(project).or_default();
        recent.retain(|(deleted_at, row)| {
            now.duration_since(*deleted_at) < Duration::from_secs(5)
                && !rows
                    .iter()
                    .any(|current| path_key(&current.path) == path_key(&row.path))
        });
        if let Some(previous) = self.previous.insert(project, rows.to_vec()) {
            for mut row in previous {
                if rows
                    .iter()
                    .any(|current| path_key(&current.path) == path_key(&row.path))
                {
                    continue;
                }
                row.deleted = true;
                row.tone = WorktreeRowTone::Deleted;
                row.size.clear();
                row.warning.clear();
                row.label = if merged.contains(&row.owner_identifier) {
                    "deleted · merged"
                } else {
                    "deleted"
                }
                .into();
                recent.push((now, row));
            }
        }
        let discard = deleted_row_window_start(recent.len());
        recent.drain(..discard);
    }
}
