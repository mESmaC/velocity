//! Browser profiles of deleted threads that could not be removed yet (SPEC.md F20, #634).
//!
//! Deleting a thread always drops its record. When its browser profile cannot be removed at
//! that moment (for example, a Windows sharing violation while a handle still lingers), it is
//! persisted here in the same write that drops the thread, and a background pass retries it.
//! Saving it means a restart never forgets a browser profile that is still on disk.

use serde::{Deserialize, Serialize};

use crate::StateStore;
use crate::threads::ThreadError;

/// A deleted thread's browser profile directory that is still on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletedBrowserProfile {
    /// The deleted thread identifier.
    pub thread: u32,
    /// The thread's fixed browser profile directory path.
    pub path: String,
}

impl DeletedBrowserProfile {
    pub fn new(thread: u32, path: String) -> Self {
        Self { thread, path }
    }
}

/// One kept browser profile, with the project it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeptBrowserProfile {
    pub project: u32,
    pub profile: DeletedBrowserProfile,
    /// Another saved thread now records this thread identifier, so it is no longer the
    /// deleted thread's profile to remove.
    pub reused: bool,
}

impl StateStore {
    /// Every deleted thread's browser profile still waiting to be removed.
    pub fn kept_browser_profiles(&self) -> Vec<KeptBrowserProfile> {
        self.state
            .projects
            .iter()
            .flat_map(|project| {
                project
                    .deleted_browser_profiles
                    .iter()
                    .map(|profile| KeptBrowserProfile {
                        project: project.id,
                        profile: profile.clone(),
                        reused: project
                            .threads
                            .iter()
                            .any(|thread| thread.id == profile.thread),
                    })
            })
            .collect()
    }

    /// Stops retrying a browser profile: it is gone, or its thread identifier is live again.
    pub fn forget_deleted_browser_profile(
        &mut self,
        project: u32,
        path: &str,
    ) -> Result<bool, ThreadError> {
        Ok(self
            .edit_project_in_place(project, |item| {
                let before = item.deleted_browser_profiles.len();
                item.deleted_browser_profiles
                    .retain(|entry| entry.path != path);
                let changed = item.deleted_browser_profiles.len() != before;
                (changed, changed)
            })?
            .unwrap_or(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threads::NewThread;
    use crate::{NewProjectRecord, ProjectLocationKind, StateStore};

    // Risk (#634): a deleted thread whose browser profile could not be removed due to a
    // transient Windows sharing violation must persist its profile cleanup request across
    // save and reload, and drop it cleanly once retry cleanup succeeds.
    #[test]
    fn deleted_browser_profile_persists_across_restart_and_clears_on_forget()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "velocity-store-deleted-browser-profile-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        std::fs::create_dir_all(&root)?;
        let mut store = StateStore::load_or_default(&root)?;
        let project = store.commit_new_project(
            NewProjectRecord {
                name: "demo".into(),
                location_kind: ProjectLocationKind::Local,
                repository_path: root.display().to_string(),
                default_branch: "master".into(),
                github_remote: None,
            },
            None,
            true,
        )?;
        let thread = store.add_thread(
            project,
            NewThread::new(
                "Browser profile thread".into(),
                "feat/browser".into(),
                root.join("worktree").display().to_string(),
                &store.settings(),
            ),
        )?;
        let profile_path = root
            .join(".velocity-browser")
            .join(format!("{project}-{thread}"))
            .display()
            .to_string();
        store.remove_deleted_thread(
            project,
            thread,
            None,
            Some(DeletedBrowserProfile::new(thread, profile_path.clone())),
        )?;

        let mut reloaded = StateStore::load_or_default(&root)?;
        let saved_project = reloaded.project(project).ok_or("saved project missing")?;
        assert!(saved_project.threads.is_empty());
        assert_eq!(
            reloaded.kept_browser_profiles(),
            vec![KeptBrowserProfile {
                project,
                profile: DeletedBrowserProfile::new(thread, profile_path.clone()),
                reused: false,
            }]
        );

        assert!(reloaded.forget_deleted_browser_profile(project, &profile_path)?);
        let after_forget = StateStore::load_or_default(&root)?;
        assert!(after_forget.kept_browser_profiles().is_empty());

        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
