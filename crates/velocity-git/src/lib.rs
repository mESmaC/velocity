//! Velocity's git and worktree manager (SPEC.md §1 Rule 5, §4 F5).
//!
//! Pure planning lives in [`path_scheme`], [`setup`] and [`lifecycle`]. Only deletion
//! has a Lean model; [`deletion_allowed`] is checked against its exported fixtures.
//! Git processes, Windows paths and setup run at the crate edge.

#![forbid(unsafe_code)]

pub mod agents_file;
pub mod agents_template;
mod changed_files;
mod command;
mod deletion;
mod dependency;
pub mod inventory;
pub mod landing;
mod lean_setup;
pub mod lifecycle;
pub mod manager;
mod merged_worktree;
pub mod path_scheme;
mod ready_pool;
mod repository;
mod review;
pub mod setup;
mod short_checkout;
mod sweep;
mod titled;

pub use changed_files::worktree_changed_files;
pub use deletion::deletion_allowed;
pub use dependency::continue_after_dependency;
pub use lean_setup::{
    LakeManifest, LakePackage, lake_project_folders, parse_manifest, parse_toolchain, prepare_lean,
};
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
pub use merged_worktree::{
    MergedWorktreeError, remove_merged_worktree, remove_untouched_worktree, verify_deletion,
    verify_thread_worktree,
};
pub use path_scheme::{
    JobKind, LocationConfig, WorktreeLeaf, WorktreePath, can_allocate_leaf, effective_root,
    leaf_for_job, plan_worktree_path, same_leaf_id,
};
pub use ready_pool::{
    FoundSlot, ReadyMark, ReadyTake, catch_up_ready_worktree, compiler_cache_env,
    create_ready_worktree, prebuild_ready_worktree, ready_slot_path, remove_ready_worktree,
    scan_ready_slots, take_ready_worktree,
};
pub use repository::{
    GitError, commit_file, current_branch, folder_is_occupied, initialize_repository,
    is_repository, main_checkout, opened_project_default_branch, publish_instructions,
};
pub use review::{ReviewCheckout, prepare_review_worktree, remove_review_worktree, review_tier};
pub use setup::{
    SetupAction, SetupPhase, WorktreeSettings, can_start_agent, default_settings,
    plan_setup_actions, resolve_cargo_target_dir,
};
pub use sweep::{prepare_sweep_worktree, resolve_commit, sweep_base};
pub use titled::{
    TitledWorktree, WorktreeName, execute_pooled_worktree_setup, execute_titled_pooled_setup,
    execute_titled_worktree_setup, rename_worktree, rename_worktree_branch,
};
