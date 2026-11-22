//! Velocity's workspace setup, inventory and git import helpers (SPEC.md §1 Rule 5, §4 F5).
//!
//! Rosen is the only version control for agent workspaces; this crate keeps workspace
//! path planning, post-take workspace setup, drive/size inventory, `AGENTS.md` editing,
//! Lean manifest helpers, git repository import inspection, and the F17 `deletion_allowed`
//! fixture guard.

#![forbid(unsafe_code)]

pub mod agents_file;
pub mod agents_template;
mod command;
mod deletion;
pub mod inventory;
mod lean_setup;
pub mod manager;
pub mod path_scheme;
mod repository;
pub mod setup;

pub use deletion::deletion_allowed;
pub use lean_setup::{
    LakeManifest, LakePackage, lake_project_folders, parse_manifest, parse_toolchain, prepare_lean,
};
pub use manager::{
    CreatedWorktree, DEFAULT_SETUP_COMMAND, DEFAULT_UNTRACKED_FILES, MAX_WORKTREE_PATH_LENGTH,
    WorktreeError, WorktreeSetupRequest, copy_configured_untracked_files, creation_transcript_step,
    finish_worktree_setup, format_leaf_folder, parse_untracked_file_list,
    resolve_project_worktree_root, resolve_worktree_directory, strip_velocity_branch_prefix,
    validate_worktree_path_length,
};
pub use path_scheme::{
    JobKind, LocationConfig, WorktreeLeaf, WorktreePath, can_allocate_leaf, effective_root,
    leaf_for_job, plan_worktree_path, same_leaf_id,
};
pub use repository::{
    GitError, commit_file, commit_files, current_branch, folder_is_occupied, is_repository,
    main_checkout, opened_project_default_branch, publish_instructions, resolve_commit,
};
pub use setup::{
    SetupAction, SetupPhase, WorktreeSettings, can_start_agent, compiler_cache_env,
    default_settings, plan_setup_actions, resolve_cargo_target_dir,
};
