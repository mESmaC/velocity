//! Persisted worktree preference edits and domain context (SPEC.md F5, F13).

use super::{
    ProjectItem, ReviewStage, StateStore, StoreError, ThreadItem, ThreadState, WorktreeRole,
};
use std::path::Path;
use velocity_core::worktree_settings::{
    WorktreeDisplayStatus, WorktreePreferenceChange, WorktreePreferences, update_preferences,
};
use velocity_core::worktree_view::{
    WorktreeContext, WorktreeOwner, WorktreeSettingsEdit, join_location, split_location,
};

pub fn default_copy_files() -> String {
    velocity_git::DEFAULT_UNTRACKED_FILES.into()
}
pub fn default_setup_command() -> String {
    velocity_git::DEFAULT_SETUP_COMMAND.into()
}

impl StateStore {
    pub fn worktree_context(&self) -> WorktreeContext {
        let project = usize::try_from(self.state.active_project_index)
            .ok()
            .and_then(|index| self.state.projects.get(index));
        let settings = &self.state.settings.worktrees;
        let root = settings
            .location_root
            .clone()
            .unwrap_or_else(|| default_worktree_root(&self.directory));
        let preferences = match project {
            Some(project) => self.worktree_preferences(project),
            None => WorktreePreferences {
                root,
                project_override: None,
                delete_local_branch: settings.delete_local_branch_on_merge,
                share_build_cache: settings.share_rust_build_cache,
                copy_files: settings.copy_files.clone(),
                setup_command: settings.setup_command.clone(),
                ready_count: settings.warm_pool_count,
            },
        };
        WorktreeContext {
            revision: self.state.revision,
            project_identifier: project.map(|project| project.id),
            project_name: project
                .map(|project| project.name.clone())
                .unwrap_or_else(|| "<project>".into()),
            repository_path: project.map(|project| project.repository_path.clone()),
            preferences,
            owners: project.map(project_owners).unwrap_or_default(),
            merged_owners: project
                .map(|project| {
                    project
                        .threads
                        .iter()
                        .filter(|thread| thread.state == ThreadState::Merged)
                        .map(|thread| thread.id)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Settings → Worktrees as they apply to `project`, active or not (F5), so background
    /// work such as a scout sweep lands where the settings page says it does.
    pub fn worktree_preferences(&self, project: &ProjectItem) -> WorktreePreferences {
        let settings = &self.state.settings.worktrees;
        WorktreePreferences {
            root: settings
                .location_root
                .clone()
                .unwrap_or_else(|| default_worktree_root(&self.directory)),
            project_override: settings.project_roots.get(&project.id).cloned(),
            delete_local_branch: settings.delete_local_branch_on_merge,
            share_build_cache: settings.share_rust_build_cache,
            copy_files: settings.copy_files.clone(),
            setup_command: settings.setup_command.clone(),
            ready_count: settings.warm_pool_count,
        }
    }

    pub fn worktree_paths(&self) -> Vec<String> {
        self.state
            .projects
            .iter()
            .flat_map(project_owners)
            .map(|owner| owner.path)
            .collect()
    }

    pub fn edit_worktree_settings(
        &mut self,
        edit: WorktreeSettingsEdit,
        project_identifier: Option<u32>,
    ) -> Result<bool, StoreError> {
        let context = self.worktree_context();
        if matches!(
            edit,
            WorktreeSettingsEdit::OverrideEnabled(_) | WorktreeSettingsEdit::ProjectRoot(_)
        ) && context.project_identifier != project_identifier
        {
            return Err(StoreError::WorktreeSetting(
                "The selected project changed; retry this edit".into(),
            ));
        }
        let change = preference_change(&context, edit)?;
        let location_changed = velocity_core::worktree_settings::changes_location(&change);
        let preferences = update_preferences(context.preferences, change);
        validate_root(&preferences.root)?;
        if let Some(root) = &preferences.project_override {
            validate_root(root)?;
        }
        let mut settings = self.settings();
        settings.worktrees.location_root = Some(preferences.root);
        settings.worktrees.delete_local_branch_on_merge = preferences.delete_local_branch;
        settings.worktrees.share_rust_build_cache = preferences.share_build_cache;
        settings.worktrees.copy_files = preferences.copy_files;
        settings.worktrees.setup_command = preferences.setup_command;
        settings.worktrees.warm_pool_count = preferences.ready_count;
        if let Some(identifier) = context.project_identifier {
            if let Some(root) = preferences.project_override {
                settings.worktrees.project_roots.insert(identifier, root);
            } else {
                settings.worktrees.project_roots.remove(&identifier);
            }
        }
        let before = self.state.clone();
        if let Err(error) = self.set_settings(settings) {
            self.state = before;
            return Err(error);
        }
        Ok(location_changed)
    }
}

// Store I/O keeps extended paths; the settings planner needs a normal drive/share prefix.
fn default_worktree_root(directory: &Path) -> String {
    let root = directory.join("worktrees").to_string_lossy().into_owned();
    if let Some(share) = root.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{share}");
    }
    root.strip_prefix(r"\\?\").unwrap_or(&root).to_owned()
}

fn validate_root(root: &str) -> Result<(), StoreError> {
    if !Path::new(root).is_absolute() || root.contains('\0') {
        return Err(StoreError::WorktreeSetting(
            "Choose an absolute folder path".into(),
        ));
    }
    Ok(())
}

fn preference_change(
    context: &WorktreeContext,
    edit: WorktreeSettingsEdit,
) -> Result<WorktreePreferenceChange, StoreError> {
    use WorktreePreferenceChange as Change;
    use WorktreeSettingsEdit as Edit;
    let preferences = &context.preferences;
    Ok(match edit {
        Edit::Root(root) => Change::Root(root),
        Edit::Drive(root) => Change::Root(join_location(
            &split_location(&root).0,
            &split_location(&preferences.root).1,
        )),
        Edit::Folder(folder) => {
            Change::Root(join_location(&split_location(&preferences.root).0, &folder))
        }
        Edit::OverrideEnabled(enabled) => {
            require_project(context)?;
            Change::ProjectOverride(if enabled {
                Some(preferences.project_override.clone().unwrap_or_else(|| {
                    Path::new(&preferences.root)
                        .join(&context.project_name)
                        .to_string_lossy()
                        .into_owned()
                }))
            } else {
                None
            })
        }
        Edit::ProjectRoot(root) => {
            require_project(context)?;
            Change::ProjectOverride(Some(root))
        }
        Edit::DeleteLocalBranch(enabled) => Change::DeleteLocalBranch(enabled),
        Edit::ShareBuildCache(enabled) => Change::ShareBuildCache(enabled),
        Edit::CopyFiles(files) => {
            velocity_git::parse_untracked_file_list(&files)
                .map_err(|error| StoreError::WorktreeSetting(error.to_string()))?;
            Change::CopyFiles(files)
        }
        Edit::SetupCommand(command) => Change::SetupCommand(command),
        Edit::ReadyCount(count) => Change::ReadyCount(count),
    })
}

fn require_project(context: &WorktreeContext) -> Result<(), StoreError> {
    context.project_identifier.map(|_| ()).ok_or_else(|| {
        StoreError::WorktreeSetting("Select a project to set its location override".into())
    })
}

pub(crate) fn project_owners(project: &ProjectItem) -> Vec<WorktreeOwner> {
    let mut owners = thread_owners(project);
    owners.extend(sweep_owners(project));
    owners
}

/// Each scout's sweep worktree (`<scout>-sweep`), listed with the Scouts role as its owner.
fn sweep_owners(project: &ProjectItem) -> Vec<WorktreeOwner> {
    project
        .duty_roles
        .iter()
        .flat_map(|role| {
            role.agents.iter().filter_map(move |agent| {
                let sweep = agent.sweep.as_ref()?;
                (!sweep.worktree.is_empty()).then(|| {
                    WorktreeOwner::new(
                        role.id,
                        format!("{} · sweep", agent.nickname),
                        sweep.worktree.clone(),
                        WorktreeDisplayStatus::Sweep,
                    )
                })
            })
        })
        .collect()
}

fn thread_owners(project: &ProjectItem) -> Vec<WorktreeOwner> {
    project
        .threads
        .iter()
        .filter_map(|thread| {
            if matches!(
                thread.state,
                ThreadState::Draft | ThreadState::Spec | ThreadState::Done | ThreadState::Merged
            ) {
                return None;
            }
            let path = thread.worktree_path.as_ref()?;
            Some(WorktreeOwner {
                identifier: thread.id,
                title: thread.title.clone(),
                path: path.clone(),
                status: thread_status(thread),
                fixing: thread.review_stage == Some(ReviewStage::Fixing),
            })
        })
        .collect()
}

fn thread_status(thread: &ThreadItem) -> WorktreeDisplayStatus {
    match thread.state {
        ThreadState::Error => WorktreeDisplayStatus::Failed,
        ThreadState::Closed => WorktreeDisplayStatus::Closed,
        _ => match thread.worktree_role {
            WorktreeRole::Review => WorktreeDisplayStatus::Review,
            WorktreeRole::Sweep => WorktreeDisplayStatus::Sweep,
            WorktreeRole::Thread if thread.state != ThreadState::Review => {
                WorktreeDisplayStatus::Working
            }
            WorktreeRole::Thread => thread
                .pull_request_number
                .map(WorktreeDisplayStatus::PullRequest)
                .unwrap_or(WorktreeDisplayStatus::Working),
        },
    }
}
