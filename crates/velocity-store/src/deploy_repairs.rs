//! Durable fix-forward repair ownership for failed `[deploy]` steps and the
//! project's remembered deploy secret names (names only, never values, #594).

use serde::{Deserialize, Serialize};
use velocity_core::deploy::validate_secret_name;

use crate::threads::{NewThread, ThreadError, insert_thread};
use crate::{StateStore, StoreError};

/// One fix-forward thread opened for a failed `[deploy.<step>]` run (#594).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRepair {
    #[serde(default)]
    pub run_id: i64,
    #[serde(default)]
    pub step: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub thread: u32,
    #[serde(default)]
    pub closed: bool,
}

impl StateStore {
    /// Allocates the fix-forward thread, its opening prompt, and its `DeployRepair`
    /// record in one save. Prevents retries and restarts from starting duplicates.
    pub fn add_deploy_repair(
        &mut self,
        project: u32,
        run_id: i64,
        step: &str,
        commit: &str,
        thread: NewThread,
        prompt: &str,
    ) -> Result<Option<u32>, ThreadError> {
        let current = self
            .project(project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if current.deploy_repairs.iter().any(|repair| {
            repair.run_id == run_id
                || (!repair.closed && repair.step == step && repair.commit == commit)
        }) {
            return Ok(None);
        }
        let mut next = self.state.clone();
        let selected = current.active_thread_id;
        let identifier = insert_thread(&mut next, project, thread, None)?;
        let current = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .ok_or(ThreadError::UnknownProject(project))?;
        if let Some(thread) = current
            .threads
            .iter_mut()
            .find(|thread| thread.id == identifier)
        {
            thread.pending_rosen_launch = Some(prompt.into());
        }
        current.active_thread_id = selected;
        current.deploy_repairs.push(DeployRepair {
            run_id,
            step: step.to_owned(),
            commit: commit.to_owned(),
            thread: identifier,
            closed: false,
        });
        self.commit(next)?;
        Ok(Some(identifier))
    }

    /// Marks open deploy repairs for `step` closed once a subsequent deploy passes.
    pub fn close_deploy_repairs(&mut self, project: u32, step: &str) -> Result<(), ThreadError> {
        self.try_edit_project(
            project,
            || ThreadError::UnknownProject(project),
            |current| {
                let mut changed = false;
                for repair in &mut current.deploy_repairs {
                    if repair.step == step && !repair.closed {
                        repair.closed = true;
                        changed = true;
                    }
                }
                Ok((changed, ()))
            },
        )
    }

    /// Remembers a validated secret name (never its value) configured for `project`
    /// so Project Settings can list it even before `rosen.toml` declares `[deploy]`.
    pub fn record_deploy_secret_name(
        &mut self,
        project: u32,
        raw_name: &str,
    ) -> Result<String, StoreError> {
        let name = validate_secret_name(raw_name).map_err(StoreError::DeploySecretName)?;
        let mut next = self.state.clone();
        if let Some(current) = next.projects.iter_mut().find(|item| item.id == project)
            && !current.deploy_secret_names.contains(&name)
        {
            current.deploy_secret_names.push(name.clone());
            current.deploy_secret_names.sort();
            self.commit(next)?;
        }
        Ok(name)
    }

    /// Removes a remembered deploy secret name from `project` after clearing it.
    pub fn remove_deploy_secret_name(
        &mut self,
        project: u32,
        name: &str,
    ) -> Result<(), StoreError> {
        let mut next = self.state.clone();
        if let Some(current) = next.projects.iter_mut().find(|item| item.id == project) {
            let before = current.deploy_secret_names.len();
            current.deploy_secret_names.retain(|entry| entry != name);
            if current.deploy_secret_names.len() != before {
                self.commit(next)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::DeployRepair;
    use crate::{ProjectItem, StateStore};

    // Risk (#594): persisted `deploy_repairs` and `deploy_secret_names` must survive
    // save and reload across restarts so failed deploys do not spawn duplicate repair
    // threads and remembered secret names remain visible in Project Settings.
    #[test]
    fn deploy_repairs_and_secret_names_survive_save_and_load()
    -> Result<(), Box<dyn std::error::Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "velocity-deploy-repairs-{}-{stamp}",
            std::process::id()
        ));
        let project_id = {
            let mut store = StateStore::load_or_default(&dir)?;
            let mut next = store.state().clone();
            let mut project =
                ProjectItem::new(1, "collapse.sh".into(), dir.to_string_lossy().into_owned());
            project.deploy_repairs.push(DeployRepair {
                run_id: 42,
                step: "fly".into(),
                commit: "a".repeat(40),
                thread: 7,
                closed: false,
            });
            next.projects.push(project);
            store.commit(next)?;
            store.record_deploy_secret_name(1, "FLY_API_TOKEN")?;
            1
        };
        let reopened = StateStore::load_or_default(&dir)?;
        let project = reopened
            .project(project_id)
            .ok_or("project missing after reload")?;
        assert_eq!(project.deploy_repairs.len(), 1);
        assert_eq!(project.deploy_repairs[0].run_id, 42);
        assert_eq!(project.deploy_repairs[0].step, "fly");
        assert_eq!(project.deploy_repairs[0].thread, 7);
        assert!(!project.deploy_repairs[0].closed);
        assert_eq!(project.deploy_secret_names, vec!["FLY_API_TOKEN"]);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
