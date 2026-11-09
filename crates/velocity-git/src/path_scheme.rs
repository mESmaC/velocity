//! Worktree path scheme and per-project leaf allocation (SPEC.md F5).
//!
//! Worktrees live under `<drive><folder>\<project>\<branch-without-vel/>` (or a
//! per-project override root). Duty reviews use `review-pr-<n>`, scout sweeps use
//! `<agent>-sweep`, and spec interviews work in the main checkout with no worktree.

/// Jobs that may request a worktree when starting (F3, F5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind<Slug, AgentId> {
    /// Spec interview works in the main checkout and gets no worktree.
    SpecInterview,
    /// Build thread, swarm member, goal swarm member, or dispatched issue (`vel/<slug>`).
    Branch(Slug),
    /// PR duty review (`review-pr-<n>`).
    DutyReview(u64),
    /// Scout sweep (`<agent>-sweep`).
    ScoutSweep(AgentId),
}

/// The project-unique worktree folder leaf (`<branch-without-vel/>`, `review-pr-<n>`,
/// or `<agent>-sweep`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeLeaf<Slug, AgentId> {
    Branch(Slug),
    DutyReview(u64),
    ScoutSweep(AgentId),
}

/// Worktree location settings for a project: either the global `<drive><folder>\<project>`
/// root or a per-project override root.
pub struct LocationConfig<Root> {
    pub global_project_root: Root,
    pub project_override: Option<Root>,
}

/// Resolved worktree path: the project root selected at creation time paired with the
/// worktree's unique leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorktreePath<Root, Slug, AgentId> {
    pub root: Root,
    pub leaf: WorktreeLeaf<Slug, AgentId>,
}

/// Resolves the effective root for new worktrees in a project: the per-project override
/// when set, otherwise `<drive><folder>\<project>`.
pub fn effective_root<Root>(config: LocationConfig<Root>) -> Root {
    match config.project_override {
        Some(override_root) => override_root,
        None => config.global_project_root,
    }
}

/// Maps a job kind to its worktree leaf, or `None` for a spec interview.
pub fn leaf_for_job<Slug, AgentId>(
    job: JobKind<Slug, AgentId>,
) -> Option<WorktreeLeaf<Slug, AgentId>> {
    match job {
        JobKind::SpecInterview => None,
        JobKind::Branch(slug) => Some(WorktreeLeaf::Branch(slug)),
        JobKind::DutyReview(pr_number) => Some(WorktreeLeaf::DutyReview(pr_number)),
        JobKind::ScoutSweep(agent_id) => Some(WorktreeLeaf::ScoutSweep(agent_id)),
    }
}

/// Plans the worktree path at creation time (`None` for a spec interview).
pub fn plan_worktree_path<Root, Slug, AgentId>(
    config: LocationConfig<Root>,
    job: JobKind<Slug, AgentId>,
) -> Option<WorktreePath<Root, Slug, AgentId>> {
    match leaf_for_job(job) {
        None => None,
        Some(leaf) => Some(WorktreePath {
            root: effective_root(config),
            leaf,
        }),
    }
}

/// Compares two numeric worktree leaf identifiers for equality without trait dispatch so
/// Charon and Aeneas translate the allocation loop directly.
pub fn same_leaf_id(first: WorktreeLeaf<u64, u64>, second: WorktreeLeaf<u64, u64>) -> bool {
    match (first, second) {
        (WorktreeLeaf::Branch(first_slug), WorktreeLeaf::Branch(second_slug)) => {
            first_slug == second_slug
        }
        (WorktreeLeaf::DutyReview(first_pr), WorktreeLeaf::DutyReview(second_pr)) => {
            first_pr == second_pr
        }
        (WorktreeLeaf::ScoutSweep(first_agent), WorktreeLeaf::ScoutSweep(second_agent)) => {
            first_agent == second_agent
        }
        _ => false,
    }
}

/// Returns `true` iff `candidate` is not already in `active_leaves`, ensuring two active
/// agents in a project never share a worktree.
pub fn can_allocate_leaf(
    active_leaves: &[WorktreeLeaf<u64, u64>],
    candidate: WorktreeLeaf<u64, u64>,
) -> bool {
    let mut index = 0;
    while index < active_leaves.len() {
        if same_leaf_id(active_leaves[index], candidate) {
            return false;
        }
        index += 1;
    }
    true
}
