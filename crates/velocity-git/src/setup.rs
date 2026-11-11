//! New worktree setup and shared Rust build cache (SPEC.md F5).
//!
//! When a worktree is created, Velocity copies configured untracked files (default
//! `.env, .env.local`), runs the project's optional post-creation setup command (empty
//! by default), and shares the Rust build cache across the project's worktrees
//! using one `CARGO_TARGET_DIR` per project. The agent starts only after setup finishes.

/// Worktree lifecycle and setup settings (F5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorktreeSettings {
    pub delete_branch_on_merge: bool,
    pub share_build_cache: bool,
    pub copy_untracked_files: bool,
    pub run_setup_command: bool,
}

/// Returns the F5 default worktree settings: delete local branch on merge (on), share the
/// Rust build cache across a project's worktrees (on), copy untracked files (on), and run
/// the post-creation setup command (on).
pub fn default_settings() -> WorktreeSettings {
    WorktreeSettings {
        delete_branch_on_merge: true,
        share_build_cache: true,
        copy_untracked_files: true,
        run_setup_command: true,
    }
}

/// Resolves `CARGO_TARGET_DIR` for a worktree: when `share_build_cache` is enabled,
/// returns `Some(shared_target_dir)` so every worktree in the project shares one build
/// cache; otherwise returns `None`.
pub fn resolve_cargo_target_dir<Path>(
    share_build_cache: bool,
    shared_target_dir: Path,
) -> Option<Path> {
    if share_build_cache {
        Some(shared_target_dir)
    } else {
        None
    }
}

/// Phases of a worktree from creation to agent launch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupPhase {
    Creating,
    CopyingFiles,
    RunningCommand,
    Ready,
    Failed,
}

/// F5: "The agent starts only after setup finishes."
pub fn can_start_agent(phase: SetupPhase) -> bool {
    matches!(phase, SetupPhase::Ready)
}

/// Ordered setup actions executed before the agent is allowed to start in a new worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupAction<Path> {
    CreateWorktreeFromMaster(Path),
    ConfigureSharedCargoTarget(Path),
    CopyUntrackedFiles(Path),
    RunSetupCommand(Path),
}

/// Produces the deterministic sequence of setup actions for a newly created worktree.
pub fn plan_setup_actions<Path: Copy>(
    settings: WorktreeSettings,
    worktree_path: Path,
    shared_target_dir: Path,
) -> Vec<SetupAction<Path>> {
    let create = SetupAction::CreateWorktreeFromMaster(worktree_path);
    let cache = SetupAction::ConfigureSharedCargoTarget(shared_target_dir);
    let copy = SetupAction::CopyUntrackedFiles(worktree_path);
    let command = SetupAction::RunSetupCommand(worktree_path);
    match (
        settings.share_build_cache,
        settings.copy_untracked_files,
        settings.run_setup_command,
    ) {
        (false, false, false) => Vec::from([create]),
        (false, false, true) => Vec::from([create, command]),
        (false, true, false) => Vec::from([create, copy]),
        (false, true, true) => Vec::from([create, copy, command]),
        (true, false, false) => Vec::from([create, cache]),
        (true, false, true) => Vec::from([create, cache, command]),
        (true, true, false) => Vec::from([create, cache, copy]),
        (true, true, true) => Vec::from([create, cache, copy, command]),
    }
}
