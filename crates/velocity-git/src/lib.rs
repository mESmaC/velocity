//! Velocity's git and worktree manager (SPEC.md §1 Rule 5, §4 F5).
//!
//! Pure, synchronous planning and state transitions live in [`path_scheme`], [`setup`],
//! and [`lifecycle`] so that Charon and Aeneas can translate them for verification in
//! `proofs/WorktreeManager/`. Concrete Windows path resolution, file copying, shared
//! `CARGO_TARGET_DIR` setup, and `git worktree` command execution live at the crate edge in
//! [`manager`]. New-project setup (`git init`, repository checks) lives in [`repository`].

#![forbid(unsafe_code)]

mod command;
pub mod inventory;
pub mod lifecycle;
pub mod manager;
mod merged_worktree;
pub mod path_scheme;
mod repository;
pub mod setup;

pub use lifecycle::{
    GitCommand, LifecycleEvent, LifecycleState, WorktreeRecord, prune_orphans_command,
};
pub use manager::{
    CreatedWorktree, DEFAULT_SETUP_COMMAND, DEFAULT_UNTRACKED_FILES, MAX_WORKTREE_PATH_LENGTH,
    WorktreeError, WorktreeSetupRequest, copy_configured_untracked_files, creation_transcript_step,
    execute_git_command, execute_git_commands, execute_worktree_setup, format_leaf_folder,
    local_branch_exists, parse_untracked_file_list, prune_orphan_worktrees,
    resolve_project_worktree_root, resolve_worktree_directory, strip_velocity_branch_prefix,
    validate_worktree_path_length,
};
pub use merged_worktree::{MergedWorktreeError, remove_merged_worktree, verify_thread_worktree};
pub use path_scheme::{
    JobKind, LocationConfig, WorktreeLeaf, WorktreePath, can_allocate_leaf, effective_root,
    leaf_for_job, plan_worktree_path, same_leaf_id,
};
pub use repository::{
    GitError, commit_file, current_branch, folder_is_occupied, initialize_repository, is_repository,
};
pub use setup::{
    SetupAction, SetupPhase, WorktreeSettings, can_start_agent, default_settings,
    plan_setup_actions, resolve_cargo_target_dir,
};
