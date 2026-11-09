module
public import Std
public section

/-!
# Worktree manager: design

Implements SPEC.md §7 M3 "Worktree manager: create / delete / prune, path scheme, setup step
(copy files, run command, shared build cache) (F5)", covering:
- §1 Rule 5: "One worktree per unit of work. Every thread and every agent works in its own
  git worktree. The worktree is deleted as soon as that work's PR merges."
- §4 F5 Worktrees (Lifecycle):
  - When a thread starts, or any agent starts a job (PR duty review, scout sweep, swarm
    member, dispatched issue), create a worktree from `master`.
  - Spec interview gets no worktree (F3, F5).
  - When its PR merges, delete the worktree, and delete the local branch too if that setting
    is on (default on).
  - When its PR is closed without merging, keep the worktree.
  - When the thread fails, keep the worktree.
  - When the user deletes the thread, delete the thread and its worktree together (F12).
  - A worktree's path is fixed when it is created. Changing the location setting affects
    only worktrees created afterwards.
- §4 F5 Worktrees (Path scheme):
  - `<drive><folder>\<project>\<branch-without-vel/>`, or per-project override root
    `<override>\<branch-without-vel/>`.
  - Duty jobs use `review-pr-<n>` and `<agent>-sweep`.
- §4 F5 Worktrees (New worktree setup):
  - Copy configured untracked files (default `.env, .env.local`), run the setup command
    (default `pnpm -C ui install`), and share the Rust build cache between a project's
    worktrees using one `CARGO_TARGET_DIR` per project (default on).
  - The agent starts only after setup finishes.
- §4 F5 Worktrees (Guards):
  - Two active agents never share a worktree.
  - Deleting a worktree removes only that worktree's own entry (`removeWorktree`).
    A repo-wide `git worktree prune` (`pruneOrphans`) runs only from the explicit
    Prune orphans action, never from per-worktree lifecycle transitions.

## Cost model

`gitOperations` counts git/filesystem mutation commands emitted by a lifecycle transition
(`removeWorktree`, `deleteBranch`, `pruneOrphans`).
- `cost_bound` proves that a single lifecycle transition emits at most 2 git operations, and
  that across any sequence of lifecycle events over a worktree's entire lifetime,
  `runLifecycle` emits at most 1 `removeWorktree` command, at most 1 `deleteBranch` command,
  and 0 `pruneOrphans` commands (at most 2 git operations total).

## Class of alternatives

`optimal` quantifies over `ValidLifecycleManagers`: every deterministic lifecycle transition
function `alt` that satisfies the F5 lifecycle specification (`MeetsLifecycleSpec`), namely:
- on `prMerged deleteLocalBranch` for a live worktree (`state ≠ .deleted`), emits
  `.removeWorktree record.path`, emits `.deleteBranch record.branch` when
  `deleteLocalBranch = true`, and transitions to `.deleted`;
- on `userDeletedThread` for a live worktree (`state ≠ .deleted`), emits
  `.removeWorktree record.path` and transitions to `.deleted`;
- on `prClosedWithoutMerge`, `threadFailed`, and `locationChanged`, keeps the worktree at its
  fixed creation path (`next.path = record.path`).

No manager in `ValidLifecycleManagers` emits fewer git operations than `stepLifecycle` on any
worktree state and event.
-/

namespace Velocity.WorktreeManager

/-! ## Path scheme and job kinds (F5) -/

/-- Jobs that may request a worktree when starting. -/
inductive JobKind (Slug AgentId : Type) where
  /-- Spec interview reads the repo in the main checkout and gets no worktree (F3, F5). -/
  | specInterview
  /-- Build thread, swarm member, goal swarm member, or dispatched issue (`vel/<slug>`). -/
  | branch (slug : Slug)
  /-- PR duty review (`review-pr-<n>`). -/
  | dutyReview (prNumber : Nat)
  /-- Scout sweep (`<agent>-sweep`). -/
  | scoutSweep (agentId : AgentId)
  deriving DecidableEq, Repr

/-- The project-unique worktree folder leaf (`<branch-without-vel/>`, `review-pr-<n>`,
or `<agent>-sweep`). -/
inductive WorktreeLeaf (Slug AgentId : Type) where
  | branch (slug : Slug)
  | dutyReview (prNumber : Nat)
  | scoutSweep (agentId : AgentId)
  deriving DecidableEq, Repr

/-- Worktree location settings for a project: either the global `<drive><folder>\<project>`
root or a per-project override root. -/
structure LocationConfig (Root : Type) where
  globalProjectRoot : Root
  projectOverride : Option Root
  deriving DecidableEq, Repr

/-- Resolved worktree path: the project root selected at creation time paired with the
worktree's unique leaf. -/
structure WorktreePath (Root Slug AgentId : Type) where
  root : Root
  leaf : WorktreeLeaf Slug AgentId
  deriving DecidableEq, Repr

variable {Root Slug AgentId Path Branch : Type}

/-- Resolves the effective root for new worktrees in a project: the per-project override if
present, otherwise `<drive><folder>\<project>`. -/
@[expose] def effectiveRoot (config : LocationConfig Root) : Root :=
  match config.projectOverride with
  | some overrideRoot => overrideRoot
  | none => config.globalProjectRoot

/-- Maps a job kind to its worktree leaf, or `none` for a spec interview. -/
@[expose] def leafForJob : JobKind Slug AgentId → Option (WorktreeLeaf Slug AgentId)
  | .specInterview => none
  | .branch slug => some (.branch slug)
  | .dutyReview prNumber => some (.dutyReview prNumber)
  | .scoutSweep agentId => some (.scoutSweep agentId)

/-- Plans the worktree path at creation time (`none` for a spec interview). -/
@[expose] def planWorktreePath (config : LocationConfig Root)
    (job : JobKind Slug AgentId) : Option (WorktreePath Root Slug AgentId) :=
  match leafForJob job with
  | none => none
  | some leaf => some { root := effectiveRoot config, leaf := leaf }

/-- Whether a new worktree leaf can be claimed without sharing an active worktree in the
same project. -/
@[expose] def canAllocateLeaf [DecidableEq (WorktreeLeaf Slug AgentId)]
    (activeLeaves : List (WorktreeLeaf Slug AgentId))
    (candidate : WorktreeLeaf Slug AgentId) : Bool :=
  !(activeLeaves.contains candidate)

/-! ## New worktree setup and shared build cache (F5) -/

/-- Worktree lifecycle and setup settings (F5). -/
structure WorktreeSettings where
  deleteBranchOnMerge : Bool
  shareBuildCache : Bool
  copyUntrackedFiles : Bool
  runSetupCommand : Bool
  deriving DecidableEq, Repr

/-- F5 default settings: delete local branch on merge (on), share Rust build cache (on),
copy `.env, .env.local` (on), run `pnpm -C ui install` (on). -/
@[expose] def defaultSettings : WorktreeSettings :=
  { deleteBranchOnMerge := true
    shareBuildCache := true
    copyUntrackedFiles := true
    runSetupCommand := true }

/-- Resolves `CARGO_TARGET_DIR` for a worktree: when `shareBuildCache` is on, returns the
project's shared target directory; otherwise `none` so Cargo uses the worktree's own target
directory. -/
@[expose] def resolveCargoTargetDir (shareBuildCache : Bool) (sharedTargetDir : Path) :
    Option Path :=
  if shareBuildCache then some sharedTargetDir else none

/-- Phases of a worktree from creation to agent launch. -/
inductive SetupPhase where
  | creating
  | copyingFiles
  | runningCommand
  | ready
  | failed
  deriving DecidableEq, Repr

/-- F5: "The agent starts only after setup finishes." -/
@[expose] def canStartAgent : SetupPhase → Bool
  | .ready => true
  | _ => false

/-- Ordered setup actions executed before the agent is allowed to start in a new worktree. -/
inductive SetupAction (Path : Type) where
  | createWorktreeFromMaster (worktreePath : Path)
  | configureSharedCargoTarget (sharedTargetDir : Path)
  | copyUntrackedFiles (worktreePath : Path)
  | runSetupCommand (worktreePath : Path)
  deriving DecidableEq, Repr

/-- Produces the deterministic sequence of setup actions for a newly created worktree. -/
@[expose] def planSetupActions (settings : WorktreeSettings) (worktreePath : Path)
    (sharedTargetDir : Path) : List (SetupAction Path) :=
  [.createWorktreeFromMaster worktreePath] ++
    (if settings.shareBuildCache then [.configureSharedCargoTarget sharedTargetDir] else []) ++
    (if settings.copyUntrackedFiles then [.copyUntrackedFiles worktreePath] else []) ++
    (if settings.runSetupCommand then [.runSetupCommand worktreePath] else [])

/-! ## Worktree lifecycle and guards (F5, F12) -/

/-- Lifecycle state of a created worktree. -/
inductive LifecycleState where
  | working
  | keptClosed
  | keptFailed
  | deleted
  deriving DecidableEq, Repr

/-- A tracked worktree record. `path` is fixed when the worktree is created. -/
structure WorktreeRecord (Path Branch : Type) where
  path : Path
  branch : Branch
  state : LifecycleState
  deriving DecidableEq, Repr

/-- Events that affect a worktree's lifecycle. -/
inductive LifecycleEvent (Path : Type) where
  /-- The worktree's PR merged into `master`. -/
  | prMerged (deleteLocalBranch : Bool)
  /-- The worktree's PR was closed without merging. -/
  | prClosedWithoutMerge
  /-- The owning thread failed on an error. -/
  | threadFailed
  /-- The user deleted the owning thread (after the 5 s undo window, F12). -/
  | userDeletedThread
  /-- The user changed the worktree location setting in Settings → Worktrees. -/
  | locationChanged (newRoot : Path)
  deriving DecidableEq, Repr

/-- Git commands that mutate worktrees or branches. -/
inductive GitCommand (Path Branch : Type) where
  /-- `git worktree remove <path>` for a single worktree entry. -/
  | removeWorktree (path : Path)
  /-- `git branch -D <branch>` for the merged worktree's local branch. -/
  | deleteBranch (branch : Branch)
  /-- Repo-wide `git worktree prune`, allowed only from the Prune orphans button. -/
  | pruneOrphans
  deriving DecidableEq, Repr

@[expose] def WorktreeRecord.new (path : Path) (branch : Branch) :
    WorktreeRecord Path Branch :=
  { path := path, branch := branch, state := .working }

/-- Pure lifecycle transition for a single worktree record. -/
@[expose] def stepLifecycle (record : WorktreeRecord Path Branch) :
    LifecycleEvent Path → WorktreeRecord Path Branch × List (GitCommand Path Branch)
  | .prMerged deleteLocalBranch =>
      match record.state with
      | .deleted => (record, [])
      | _ =>
          let commands :=
            if deleteLocalBranch then
              [.removeWorktree record.path, .deleteBranch record.branch]
            else
              [.removeWorktree record.path]
          ({ record with state := .deleted }, commands)
  | .prClosedWithoutMerge =>
      match record.state with
      | .deleted => (record, [])
      | _ => ({ record with state := .keptClosed }, [])
  | .threadFailed =>
      match record.state with
      | .deleted => (record, [])
      | _ => ({ record with state := .keptFailed }, [])
  | .userDeletedThread =>
      match record.state with
      | .deleted => (record, [])
      | _ => ({ record with state := .deleted }, [.removeWorktree record.path])
  | .locationChanged _ =>
      (record, [])

/-- Runs a sequence of lifecycle events on a worktree record. -/
@[expose] def runLifecycle (record : WorktreeRecord Path Branch) :
    List (LifecycleEvent Path) → WorktreeRecord Path Branch × List (GitCommand Path Branch)
  | [] => (record, [])
  | event :: rest =>
      let (next, commands) := stepLifecycle record event
      let (final, laterCommands) := runLifecycle next rest
      (final, commands ++ laterCommands)

/-- Explicit command for the Settings → Worktrees **Prune orphans** button. -/
@[expose] def pruneOrphansCommand : GitCommand Path Branch := .pruneOrphans

/-! ## Cost model and class of alternatives -/

/-- Cost model: number of git/filesystem mutation commands emitted. -/
@[expose] def gitOperations (commands : List (GitCommand Path Branch)) : Nat :=
  commands.length

@[expose] def removeWorktreeCount : List (GitCommand Path Branch) → Nat
  | [] => 0
  | .removeWorktree _ :: rest => removeWorktreeCount rest + 1
  | _ :: rest => removeWorktreeCount rest

@[expose] def deleteBranchCount : List (GitCommand Path Branch) → Nat
  | [] => 0
  | .deleteBranch _ :: rest => deleteBranchCount rest + 1
  | _ :: rest => deleteBranchCount rest

@[expose] def pruneOrphansCount : List (GitCommand Path Branch) → Nat
  | [] => 0
  | .pruneOrphans :: rest => pruneOrphansCount rest + 1
  | _ :: rest => pruneOrphansCount rest

/-- Specification that any valid single-step worktree lifecycle manager must satisfy (F5). -/
@[expose] def MeetsLifecycleSpec
    (manager : WorktreeRecord Path Branch → LifecycleEvent Path →
      WorktreeRecord Path Branch × List (GitCommand Path Branch)) : Prop :=
  (∀ record deleteLocalBranch, record.state ≠ .deleted →
    let (next, commands) := manager record (.prMerged deleteLocalBranch)
    next.state = .deleted ∧
      next.path = record.path ∧
      GitCommand.removeWorktree record.path ∈ commands ∧
      (deleteLocalBranch = true → GitCommand.deleteBranch record.branch ∈ commands)) ∧
  (∀ record, record.state ≠ .deleted →
    let (next, commands) := manager record .prClosedWithoutMerge
    next.state = .keptClosed ∧ next.path = record.path ∧
      GitCommand.removeWorktree record.path ∉ commands) ∧
  (∀ record, record.state ≠ .deleted →
    let (next, commands) := manager record .threadFailed
    next.state = .keptFailed ∧ next.path = record.path ∧
      GitCommand.removeWorktree record.path ∉ commands) ∧
  (∀ record, record.state ≠ .deleted →
    let (next, commands) := manager record .userDeletedThread
    next.state = .deleted ∧
      next.path = record.path ∧
      GitCommand.removeWorktree record.path ∈ commands) ∧
  (∀ record newRoot,
    let (next, commands) := manager record (.locationChanged newRoot)
    next = record ∧ GitCommand.removeWorktree record.path ∉ commands) ∧
  (∀ record event,
    GitCommand.pruneOrphans ∉ (manager record event).2)

/-! ## Design theorems -/

/-- F3, F5: A spec interview never creates a worktree. -/
theorem spec_interview_no_worktree (config : LocationConfig Root) :
    planWorktreePath config (.specInterview : JobKind Slug AgentId) = none := by
  rfl

/-- F5: Every non-interview job (thread/swarm/dispatch branch, PR duty review, scout sweep)
plans a worktree under the effective project root. -/
theorem non_interview_plans_worktree (config : LocationConfig Root)
    (job : JobKind Slug AgentId) (notSpec : job ≠ .specInterview) :
    ∃ leaf, planWorktreePath config job =
      some { root := effectiveRoot config, leaf := leaf } := by
  cases job with
  | specInterview => exact absurd rfl notSpec
  | branch slug => exact ⟨.branch slug, rfl⟩
  | dutyReview prNumber => exact ⟨.dutyReview prNumber, rfl⟩
  | scoutSweep agentId => exact ⟨.scoutSweep agentId, rfl⟩

private def jobOfLeaf : WorktreeLeaf Slug AgentId → JobKind Slug AgentId
  | .branch slug => .branch slug
  | .dutyReview prNumber => .dutyReview prNumber
  | .scoutSweep agentId => .scoutSweep agentId

private theorem jobOfLeaf_leafForJob (job : JobKind Slug AgentId)
    (leaf : WorktreeLeaf Slug AgentId) (hasLeaf : leafForJob job = some leaf) :
    jobOfLeaf leaf = job := by
  cases job <;> simp only [leafForJob, reduceCtorEq, Option.some.injEq] at hasLeaf
  all_goals
    subst hasLeaf
    rfl

/-- F5 Guard: Two distinct jobs in the same project never collide on their worktree path. -/
theorem worktree_paths_disjoint (config : LocationConfig Root)
    (firstJob secondJob : JobKind Slug AgentId)
    (firstPath secondPath : WorktreePath Root Slug AgentId)
    (firstPlanned : planWorktreePath config firstJob = some firstPath)
    (secondPlanned : planWorktreePath config secondJob = some secondPath)
    (distinctJobs : firstJob ≠ secondJob) :
    firstPath ≠ secondPath := by
  intro samePath
  subst samePath
  cases firstLeaf : leafForJob firstJob with
  | none => simp [planWorktreePath, firstLeaf] at firstPlanned
  | some leafA =>
    cases secondLeaf : leafForJob secondJob with
    | none => simp [planWorktreePath, secondLeaf] at secondPlanned
    | some leafB =>
      have eqA : { root := effectiveRoot config, leaf := leafA } = firstPath := by
        simpa [planWorktreePath, firstLeaf] using firstPlanned
      have eqB : { root := effectiveRoot config, leaf := leafB } = firstPath := by
        simpa [planWorktreePath, secondLeaf] using secondPlanned
      have sameLeaf : leafA = leafB := (WorktreePath.mk.inj (eqA.trans eqB.symm)).2
      subst sameLeaf
      have jobEq : firstJob = secondJob :=
        (jobOfLeaf_leafForJob firstJob leafA firstLeaf).symm.trans
          (jobOfLeaf_leafForJob secondJob leafA secondLeaf)
      exact distinctJobs jobEq

/-- F5: Two agents never share an active worktree leaf when allocation checks
`canAllocateLeaf`. -/
theorem allocation_prevents_sharing [DecidableEq (WorktreeLeaf Slug AgentId)]
    (activeLeaves : List (WorktreeLeaf Slug AgentId))
    (candidate : WorktreeLeaf Slug AgentId) :
    canAllocateLeaf activeLeaves candidate = true ↔ candidate ∉ activeLeaves := by
  simp [canAllocateLeaf]

/-- F5: Sharing the build cache uses one `CARGO_TARGET_DIR` per project when enabled, and
none when disabled. -/
theorem shared_cache_uses_project_target_dir (shareBuildCache : Bool)
    (sharedTargetDir : Path) :
    (resolveCargoTargetDir shareBuildCache sharedTargetDir = some sharedTargetDir ↔
      shareBuildCache = true) ∧
    (resolveCargoTargetDir shareBuildCache sharedTargetDir = none ↔
      shareBuildCache = false) := by
  cases shareBuildCache <;> simp [resolveCargoTargetDir]

/-- F5: The agent starts only after setup finishes (`SetupPhase.ready`). -/
theorem agent_starts_only_after_setup (phase : SetupPhase) :
    canStartAgent phase = true ↔ phase = .ready := by
  cases phase <;> simp [canStartAgent]

/-- F5: Changing the worktree location never moves or mutates an existing worktree. -/
theorem location_change_preserves_existing (record : WorktreeRecord Path Branch)
    (newRoot : Path) :
    stepLifecycle record (.locationChanged newRoot) = (record, []) := by
  rfl

/-- F5: Across any sequence of lifecycle events, a worktree's creation path and branch
never change. -/
theorem stepLifecycle_preserves_identity (record : WorktreeRecord Path Branch)
    (event : LifecycleEvent Path) :
    (stepLifecycle record event).1.path = record.path ∧
      (stepLifecycle record event).1.branch = record.branch := by
  cases event <;> cases state : record.state <;> simp [stepLifecycle, state]

/-- F5: Merging a live worktree deletes its worktree in that step, and deletes the local
branch iff `deleteLocalBranch` is enabled. -/
theorem merge_deletes_worktree (record : WorktreeRecord Path Branch)
    (deleteLocalBranch : Bool) (live : record.state ≠ .deleted) :
    (stepLifecycle record (.prMerged deleteLocalBranch)).1.state = .deleted ∧
      GitCommand.removeWorktree record.path ∈
        (stepLifecycle record (.prMerged deleteLocalBranch)).2 ∧
      (GitCommand.deleteBranch record.branch ∈
        (stepLifecycle record (.prMerged deleteLocalBranch)).2 ↔
          deleteLocalBranch = true) := by
  cases state : record.state <;> try exact absurd state live
  all_goals
    cases deleteLocalBranch <;> simp [stepLifecycle, state]

/-- F5: Closed and failed threads keep their worktree and emit no removal command. -/
theorem closed_and_failed_keep_worktree (record : WorktreeRecord Path Branch)
    (live : record.state ≠ .deleted) :
    stepLifecycle record .prClosedWithoutMerge =
      ({ record with state := .keptClosed }, []) ∧
    stepLifecycle record .threadFailed =
      ({ record with state := .keptFailed }, []) := by
  cases state : record.state <;> try exact absurd state live
  all_goals simp [stepLifecycle, state]

/-- F5, F12: Deleting the thread deletes its worktree from any live state (`working`,
`keptClosed`, or `keptFailed`). -/
theorem user_delete_removes_worktree (record : WorktreeRecord Path Branch)
    (live : record.state ≠ .deleted) :
    stepLifecycle record .userDeletedThread =
      ({ record with state := .deleted }, [.removeWorktree record.path]) := by
  cases state : record.state <;> try exact absurd state live
  all_goals simp [stepLifecycle, state]

/-- F5 Guard: Per-worktree lifecycle transitions never run a repo-wide `git worktree prune`
 and only ever remove `record.path`. -/
theorem lifecycle_never_prunes_or_touches_other_worktrees
    (record : WorktreeRecord Path Branch) (event : LifecycleEvent Path)
    (otherPath : Path) (distinct : otherPath ≠ record.path) :
    GitCommand.pruneOrphans ∉ (stepLifecycle record event).2 ∧
      GitCommand.removeWorktree otherPath ∉ (stepLifecycle record event).2 := by
  cases event with
  | prMerged deleteLocalBranch =>
    cases state : record.state <;> cases deleteLocalBranch <;>
      simp [stepLifecycle, state, distinct]
  | prClosedWithoutMerge =>
    cases state : record.state <;> simp [stepLifecycle, state]
  | threadFailed =>
    cases state : record.state <;> simp [stepLifecycle, state]
  | userDeletedThread =>
    cases state : record.state <;> simp [stepLifecycle, state, distinct]
  | locationChanged newRoot =>
    simp [stepLifecycle]

/-- `stepLifecycle` satisfies `MeetsLifecycleSpec`. -/
theorem stepLifecycle_meets_spec :
    MeetsLifecycleSpec (stepLifecycle (Path := Path) (Branch := Branch)) := by
  refine ⟨?_, ?_, ?_, ?_, ?_, ?_⟩
  · intro record deleteLocalBranch live
    have merged := merge_deletes_worktree record deleteLocalBranch live
    have identity := stepLifecycle_preserves_identity record (.prMerged deleteLocalBranch)
    exact ⟨merged.1, identity.1, merged.2.1, merged.2.2.mpr⟩
  · intro record live
    have kept := (closed_and_failed_keep_worktree record live).1
    simp [kept]
  · intro record live
    have kept := (closed_and_failed_keep_worktree record live).2
    simp [kept]
  · intro record live
    have removed := user_delete_removes_worktree record live
    simp [removed]
  · intro record newRoot
    simp [stepLifecycle]
  · intro record event
    cases event with
    | prMerged deleteLocalBranch =>
      cases state : record.state <;> cases deleteLocalBranch <;> simp [stepLifecycle, state]
    | prClosedWithoutMerge =>
      cases state : record.state <;> simp [stepLifecycle, state]
    | threadFailed =>
      cases state : record.state <;> simp [stepLifecycle, state]
    | userDeletedThread =>
      cases state : record.state <;> simp [stepLifecycle, state]
    | locationChanged newRoot =>
      simp [stepLifecycle]

/-- Full design correctness theorem (`correct`):
1. Spec interviews get no worktree, while every non-interview job plans a unique per-project
   worktree path under the effective root.
2. Active leaf allocation prevents two agents from sharing a worktree.
3. Shared build cache uses one project-scoped `CARGO_TARGET_DIR` iff enabled, and agents
   start only after setup reaches `ready`.
4. `stepLifecycle` satisfies `MeetsLifecycleSpec` (merge deletes worktree and optional
   branch, close and failure keep the worktree, user delete removes the worktree, location
   changes leave existing worktrees untouched, and lifecycle events never run `pruneOrphans`). -/
theorem correct [DecidableEq (WorktreeLeaf Slug AgentId)] :
    (∀ config : LocationConfig Root,
      planWorktreePath config (.specInterview : JobKind Slug AgentId) = none) ∧
    (∀ (config : LocationConfig Root) (firstJob secondJob : JobKind Slug AgentId)
        (firstPath secondPath : WorktreePath Root Slug AgentId),
      planWorktreePath config firstJob = some firstPath →
      planWorktreePath config secondJob = some secondPath →
      firstJob ≠ secondJob →
      firstPath ≠ secondPath) ∧
    (∀ (activeLeaves : List (WorktreeLeaf Slug AgentId))
        (candidate : WorktreeLeaf Slug AgentId),
      canAllocateLeaf activeLeaves candidate = true ↔ candidate ∉ activeLeaves) ∧
    (∀ (shareBuildCache : Bool) (sharedTargetDir : Path),
      resolveCargoTargetDir shareBuildCache sharedTargetDir = some sharedTargetDir ↔
        shareBuildCache = true) ∧
    (∀ phase : SetupPhase, canStartAgent phase = true ↔ phase = .ready) ∧
    MeetsLifecycleSpec (stepLifecycle (Path := Path) (Branch := Branch)) :=
  ⟨spec_interview_no_worktree,
   worktree_paths_disjoint,
   allocation_prevents_sharing,
   fun shareBuildCache sharedTargetDir =>
     (shared_cache_uses_project_target_dir shareBuildCache sharedTargetDir).1,
   agent_starts_only_after_setup,
   stepLifecycle_meets_spec⟩

/-! ## Cost bound -/

private theorem deleted_absorbs_step (record : WorktreeRecord Path Branch)
    (event : LifecycleEvent Path) (isDeleted : record.state = .deleted) :
    stepLifecycle record event = (record, []) := by
  cases event <;> simp [stepLifecycle, isDeleted]

private theorem deleted_absorbs_run (record : WorktreeRecord Path Branch)
    (events : List (LifecycleEvent Path)) (isDeleted : record.state = .deleted) :
    runLifecycle record events = (record, []) := by
  induction events with
  | nil => rfl
  | cons event rest inductionHypothesis =>
    simp [runLifecycle, deleted_absorbs_step record event isDeleted, inductionHypothesis]

private theorem deleted_run_snd (path : Path) (branch : Branch)
    (events : List (LifecycleEvent Path)) :
    (runLifecycle { path := path, branch := branch, state := .deleted } events).2 = [] := by
  rw [deleted_absorbs_run { path := path, branch := branch, state := .deleted } events rfl]

/-- Across any finite trace of lifecycle events, `runLifecycle` removes the worktree at most
once, deletes its branch at most once, never runs `git worktree prune`, and executes at most
2 git operations in total. -/
theorem cost_bound (record : WorktreeRecord Path Branch)
    (events : List (LifecycleEvent Path)) :
    let commands := (runLifecycle record events).2
    removeWorktreeCount commands ≤ 1 ∧
      deleteBranchCount commands ≤ 1 ∧
      pruneOrphansCount commands = 0 ∧
      gitOperations commands ≤ 2 := by
  induction events generalizing record with
  | nil =>
    simp [runLifecycle, removeWorktreeCount, deleteBranchCount, pruneOrphansCount, gitOperations]
  | cons event rest inductionHypothesis =>
    by_cases isDeleted : record.state = .deleted
    · rw [deleted_absorbs_run record (event :: rest) isDeleted]
      simp [removeWorktreeCount, deleteBranchCount, pruneOrphansCount, gitOperations]
    · cases event with
      | prMerged deleteLocalBranch =>
        cases state : record.state <;> try exact absurd state isDeleted
        all_goals
          cases deleteLocalBranch <;>
            simp [runLifecycle, stepLifecycle, state, deleted_run_snd,
              removeWorktreeCount, deleteBranchCount, pruneOrphansCount, gitOperations]
      | userDeletedThread =>
        have removed := user_delete_removes_worktree record isDeleted
        simp [runLifecycle, removed, deleted_run_snd,
          removeWorktreeCount, deleteBranchCount, pruneOrphansCount, gitOperations]
      | prClosedWithoutMerge =>
        have kept := (closed_and_failed_keep_worktree record isDeleted).1
        simpa [runLifecycle, kept] using
          inductionHypothesis { record with state := .keptClosed }
      | threadFailed =>
        have kept := (closed_and_failed_keep_worktree record isDeleted).2
        simpa [runLifecycle, kept] using
          inductionHypothesis { record with state := .keptFailed }
      | locationChanged newRoot =>
        simpa [runLifecycle, stepLifecycle] using inductionHypothesis record

/-! ## Optimality -/

private theorem two_le_length_of_distinct_mem {α : Type} {first second : α} {items : List α}
    (distinct : first ≠ second) (firstIn : first ∈ items) (secondIn : second ∈ items) :
    2 ≤ items.length := by
  cases items with
  | nil => cases firstIn
  | cons head tail =>
    cases tail with
    | nil =>
      simp only [List.mem_singleton] at firstIn secondIn
      exact absurd (firstIn.trans secondIn.symm) distinct
    | cons secondHead rest =>
      simp

/-- Optimality (`optimal`):
- **Cost model:** `gitOperations` (the number of git/filesystem mutation commands emitted).
- **Class of alternatives:** all lifecycle managers `alternative` satisfying
  `MeetsLifecycleSpec`.
For every worktree record `record` and every lifecycle event `event`, `stepLifecycle` emits
no more git operations than any `alternative` meeting `MeetsLifecycleSpec`. -/
theorem optimal
    (alternative : WorktreeRecord Path Branch → LifecycleEvent Path →
      WorktreeRecord Path Branch × List (GitCommand Path Branch))
    (inClass : MeetsLifecycleSpec alternative)
    (record : WorktreeRecord Path Branch)
    (event : LifecycleEvent Path) :
    gitOperations (stepLifecycle record event).2 ≤
      gitOperations (alternative record event).2 := by
  obtain ⟨onMerge, _, _, onUserDelete, _, _⟩ := inClass
  by_cases isDeleted : record.state = .deleted
  · rw [deleted_absorbs_step record event isDeleted]
    simp [gitOperations]
  · cases event with
    | prClosedWithoutMerge =>
      rw [(closed_and_failed_keep_worktree record isDeleted).1]
      simp [gitOperations]
    | threadFailed =>
      rw [(closed_and_failed_keep_worktree record isDeleted).2]
      simp [gitOperations]
    | locationChanged newRoot =>
      simp [stepLifecycle, gitOperations]
    | userDeletedThread =>
      rw [user_delete_removes_worktree record isDeleted]
      have ⟨_, _, hasRemove⟩ := onUserDelete record isDeleted
      have pos : 1 ≤ (alternative record .userDeletedThread).2.length :=
        List.length_pos_of_mem hasRemove
      simpa [gitOperations] using pos
    | prMerged deleteLocalBranch =>
      have ⟨_, _, hasRemove, hasDeleteBranch⟩ := onMerge record deleteLocalBranch isDeleted
      cases state : record.state <;> try exact absurd state isDeleted
      all_goals
        cases deleteLocalBranch with
        | false =>
          have pos : 1 ≤ (alternative record (.prMerged false)).2.length :=
            List.length_pos_of_mem hasRemove
          simpa [stepLifecycle, state, gitOperations] using pos
        | true =>
          have branchMem := hasDeleteBranch rfl
          have atLeastTwo : 2 ≤ (alternative record (.prMerged true)).2.length :=
            two_le_length_of_distinct_mem (by rintro ⟨⟩) hasRemove branchMem
          simpa [stepLifecycle, state, gitOperations] using atLeastTwo

#print axioms spec_interview_no_worktree
#print axioms non_interview_plans_worktree
#print axioms worktree_paths_disjoint
#print axioms allocation_prevents_sharing
#print axioms shared_cache_uses_project_target_dir
#print axioms agent_starts_only_after_setup
#print axioms location_change_preserves_existing
#print axioms stepLifecycle_preserves_identity
#print axioms merge_deletes_worktree
#print axioms closed_and_failed_keep_worktree
#print axioms user_delete_removes_worktree
#print axioms lifecycle_never_prunes_or_touches_other_worktrees
#print axioms stepLifecycle_meets_spec
#print axioms correct
#print axioms cost_bound
#print axioms optimal

end Velocity.WorktreeManager
