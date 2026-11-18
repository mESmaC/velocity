//! The durable project and thread transition for the one-way startup migration.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::{AgentActivity, ProjectItem, StateStore, StoreError, VersionControl};

// Records saved before the version-control choice existed were git projects.
pub(crate) fn legacy_version_control() -> VersionControl {
    VersionControl::Git
}

pub fn migration_waiting(project: &ProjectItem) -> bool {
    project.threads.iter().any(|thread| {
        thread.startup_recovery_pending
            || matches!(
                thread.activity,
                AgentActivity::Thinking | AgentActivity::Executing | AgentActivity::Stalled
            )
    })
}

impl StateStore {
    /// Commit only if nothing in this project's saved record changed during import.
    /// A launch racing the background worker therefore keeps its original git mode.
    pub fn finish_rosen_migration(
        &mut self,
        expected: &ProjectItem,
        migrated: &BTreeMap<u32, (String, PathBuf)>,
        last_import_event: u64,
    ) -> Result<bool, StoreError> {
        if expected.version_control != VersionControl::Git
            || self.project(expected.id) != Some(expected)
            || migration_waiting(expected)
        {
            return Ok(false);
        }
        let mut next = self.state.clone();
        if let Some(project) = next
            .projects
            .iter_mut()
            .find(|project| project.id == expected.id)
        {
            project.version_control = VersionControl::Rosen;
            project.rosen_migration_notice = None;
            for thread in &mut project.threads {
                if let Some((change, path)) = migrated.get(&thread.id) {
                    let path = path.to_string_lossy().into_owned();
                    thread.worktree_path_id = Some(path.clone());
                    thread.worktree_path = Some(path);
                    thread.rosen_change = Some(change.clone());
                    thread.rosen_import_event = last_import_event;
                }
            }
        }
        self.store_snapshot(next)?;
        Ok(true)
    }

    pub fn save_migration_notice(
        &mut self,
        identifier: u32,
        notice: String,
    ) -> Result<(), StoreError> {
        if self.project(identifier).is_none_or(|project| {
            project.version_control != VersionControl::Git
                || project.rosen_migration_notice.as_ref() == Some(&notice)
        }) {
            return Ok(());
        }
        let mut next = self.state.clone();
        if let Some(project) = next
            .projects
            .iter_mut()
            .find(|project| project.id == identifier)
        {
            project.rosen_migration_notice = Some(notice);
        }
        self.store_snapshot(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ThreadItem, ThreadState};
    use velocity_core::thread_view_model::TranscriptBlock;

    // Risk (#419): switching a saved project must retain its history and checkout paths
    // across restart, and an older record must still enter the migration path.
    #[test]
    fn migrated_record_survives_save_and_load() -> Result<(), Box<dyn std::error::Error>> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("velocity-migration-{timestamp}"));
        let mut project = ProjectItem::new(1, "Saved project".into(), r"C:\project one".into());
        project.version_control = VersionControl::Git;
        project.rosen_migration_notice = Some("Waiting for clean worktrees".into());
        let mut thread = ThreadItem::new(10, "Retained history".into());
        thread.state = ThreadState::Closed;
        thread.worktree_path = Some(r"X:\workspaces\project one\old thread".into());
        thread.transcript = std::sync::Arc::new(vec![TranscriptBlock::AgentMessage {
            paragraphs: vec!["Original conversation".into()],
        }]);
        project.threads.push(thread);
        let project = project.normalize();
        let mut store = StateStore::load_or_default(&directory)?;
        store.state.projects.push(project.clone());
        let workspace = PathBuf::from(r"X:\workspaces\project one\migrated");
        let change = String::from("klmnopqrstuvwxyzklmnopqrstuvwxyz");
        let migrated = BTreeMap::from([(10, (change.clone(), workspace.clone()))]);
        assert!(store.finish_rosen_migration(&project, &migrated, 83)?);
        drop(store);
        let loaded = StateStore::load_or_default(&directory)?;
        let saved = loaded.project(1).ok_or("missing migrated project")?;
        assert_eq!(saved.version_control, VersionControl::Rosen);
        let mut expected_thread = project.threads[0].clone();
        expected_thread.worktree_path = Some(workspace.to_string_lossy().into_owned());
        expected_thread.worktree_path_id = expected_thread.worktree_path.clone();
        expected_thread.rosen_change = Some(change);
        expected_thread.rosen_import_event = 83;
        assert_eq!(saved.threads, vec![expected_thread]);
        assert_eq!(saved.rosen_migration_notice, None);
        let mut legacy = serde_json::to_value(saved)?;
        let fields = legacy.as_object_mut().ok_or("missing record fields")?;
        fields.remove("version_control");
        fields.remove("rosen_migration_notice");
        let legacy: ProjectItem = serde_json::from_value(legacy)?;
        assert_eq!(legacy.version_control, VersionControl::Git);
        assert_eq!(legacy.rosen_migration_notice, None);
        drop(loaded);
        std::fs::remove_file(directory.join(crate::STATE_FILE_NAME))?;
        std::fs::remove_dir(directory)?;
        Ok(())
    }
}
