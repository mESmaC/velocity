module
public import WorktreeManager.Design
public import WorktreeManager.Generated.Funs
public section

/-!
# Worktree manager: confidence

Proof level: **code**. `Generated/` is the Charon + Aeneas translation of
`crates/velocity-git` (`proofs/scripts/Export-Rust.ps1 -CratePath crates/velocity-git
-Feature WorktreeManager`). Each theorem below shows that a translated Rust function never
fails and returns what the design in `Design.lean` returns, through the `lift*` maps from the
translated types to the design's types.

`run_lifecycle_refines` lifts `step_lifecycle_refines` to any sequence of lifecycle events fed
to `WorktreeRecord::step` in turn. The design theorems (`correct`, `cost_bound`, `optimal`)
therefore hold of the Rust code.
-/

namespace Velocity.WorktreeManager

open Aeneas Aeneas.Std Result ControlFlow
open Generated

variable {Root Slug AgentId Path Branch : Type}

/-! ## From the translated types to the design's -/

@[expose] def liftJobKind : path_scheme.JobKind Slug AgentId → JobKind Slug AgentId
  | .SpecInterview => .specInterview
  | .Branch slug => .branch slug
  | .DutyReview prNumber => .dutyReview prNumber.val
  | .ScoutSweep agentId => .scoutSweep agentId

@[expose] def liftWorktreeLeaf : path_scheme.WorktreeLeaf Slug AgentId → WorktreeLeaf Slug AgentId
  | .Branch slug => .branch slug
  | .DutyReview prNumber => .dutyReview prNumber.val
  | .ScoutSweep agentId => .scoutSweep agentId

@[expose] def liftNumericLeaf : path_scheme.WorktreeLeaf Std.U64 Std.U64 → WorktreeLeaf Nat Nat
  | .Branch slug => .branch slug.val
  | .DutyReview prNumber => .dutyReview prNumber.val
  | .ScoutSweep agentId => .scoutSweep agentId.val

@[expose] def liftLocationConfig (config : path_scheme.LocationConfig Root) : LocationConfig Root :=
  { globalProjectRoot := config.global_project_root
    projectOverride := config.project_override }

@[expose] def liftWorktreePath (path : path_scheme.WorktreePath Root Slug AgentId) :
    WorktreePath Root Slug AgentId :=
  { root := path.root
    leaf := liftWorktreeLeaf path.leaf }

@[expose] def liftWorktreeSettings (settings : setup.WorktreeSettings) : WorktreeSettings :=
  { deleteBranchOnMerge := settings.delete_branch_on_merge
    shareBuildCache := settings.share_build_cache
    copyUntrackedFiles := settings.copy_untracked_files
    runSetupCommand := settings.run_setup_command }

@[expose] def liftSetupPhase : setup.SetupPhase → SetupPhase
  | .Creating => .creating
  | .CopyingFiles => .copyingFiles
  | .RunningCommand => .runningCommand
  | .Ready => .ready
  | .Failed => .failed

@[expose] def liftSetupAction : setup.SetupAction Path → SetupAction Path
  | .CreateWorktreeFromMaster worktreePath => .createWorktreeFromMaster worktreePath
  | .ConfigureSharedCargoTarget sharedTargetDir => .configureSharedCargoTarget sharedTargetDir
  | .CopyUntrackedFiles worktreePath => .copyUntrackedFiles worktreePath
  | .RunSetupCommand worktreePath => .runSetupCommand worktreePath

@[expose] def liftLifecycleState : lifecycle.LifecycleState → LifecycleState
  | .Working => .working
  | .KeptClosed => .keptClosed
  | .KeptFailed => .keptFailed
  | .Deleted => .deleted

@[expose] def liftWorktreeRecord (record : lifecycle.WorktreeRecord Path Branch) :
    WorktreeRecord Path Branch :=
  { path := record.path
    branch := record.branch
    state := liftLifecycleState record.state }

@[expose] def liftLifecycleEvent : lifecycle.LifecycleEvent Path → LifecycleEvent Path
  | .PrMerged deleteLocalBranch => .prMerged deleteLocalBranch
  | .PrClosedWithoutMerge => .prClosedWithoutMerge
  | .ThreadFailed => .threadFailed
  | .UserDeletedThread => .userDeletedThread
  | .LocationChanged newRoot => .locationChanged newRoot

@[expose] def liftGitCommand : lifecycle.GitCommand Path Branch → GitCommand Path Branch
  | .RemoveWorktree path => .removeWorktree path
  | .DeleteBranch branch => .deleteBranch branch
  | .PruneOrphans => .pruneOrphans

/-! ## Path scheme and leaf allocation -/

theorem effective_root_refines (config : path_scheme.LocationConfig Root) :
    path_scheme.effective_root config ⦃ root =>
      root = effectiveRoot (liftLocationConfig config) ⦄ := by
  cases override : config.project_override <;>
    simp [path_scheme.effective_root, effectiveRoot, liftLocationConfig, override]

theorem leaf_for_job_refines (job : path_scheme.JobKind Slug AgentId) :
    path_scheme.leaf_for_job job ⦃ leaf =>
      leaf.map liftWorktreeLeaf = leafForJob (liftJobKind job) ⦄ := by
  cases job <;>
    simp [path_scheme.leaf_for_job, leafForJob, liftJobKind, liftWorktreeLeaf]

theorem plan_worktree_path_refines (config : path_scheme.LocationConfig Root)
    (job : path_scheme.JobKind Slug AgentId) :
    path_scheme.plan_worktree_path config job ⦃ planned =>
      planned.map liftWorktreePath =
        planWorktreePath (liftLocationConfig config) (liftJobKind job) ⦄ := by
  unfold path_scheme.plan_worktree_path
  cases job <;> cases override : config.project_override <;>
    simp [path_scheme.leaf_for_job, path_scheme.effective_root, planWorktreePath,
      leafForJob, effectiveRoot, liftLocationConfig, liftJobKind, liftWorktreePath,
      liftWorktreeLeaf, override]

private theorem u64_val_inj (first second : Std.U64) :
    first.val = second.val ↔ first = second :=
  ⟨fun _ => by scalar_tac, fun same => by subst same; rfl⟩

theorem same_leaf_id_refines (first second : path_scheme.WorktreeLeaf Std.U64 Std.U64) :
    path_scheme.same_leaf_id first second ⦃ matched =>
      matched = decide (liftNumericLeaf first = liftNumericLeaf second) ⦄ := by
  cases first <;> cases second <;>
    simp [path_scheme.same_leaf_id, liftNumericLeaf, u64_val_inj]

private theorem drop_cons_getElem {α : Type} (list : List α) (index : Nat)
    (bound : index < list.length) :
    list.drop index = list[index] :: list.drop (index + 1) := by
  rw [List.drop_eq_getElem_cons bound]

theorem can_allocate_leaf_refines
    (activeLeaves : Slice (path_scheme.WorktreeLeaf Std.U64 Std.U64))
    (candidate : path_scheme.WorktreeLeaf Std.U64 Std.U64) :
    path_scheme.can_allocate_leaf activeLeaves candidate ⦃ allowed =>
      allowed = canAllocateLeaf (activeLeaves.val.map liftNumericLeaf) (liftNumericLeaf candidate) ⦄ := by
  unfold path_scheme.can_allocate_leaf path_scheme.can_allocate_leaf_loop
  apply loop.spec_decr_nat (fun index => activeLeaves.length - index.val)
    (fun index => index.val ≤ activeLeaves.length ∧
      canAllocateLeaf ((activeLeaves.val.map liftNumericLeaf).drop index.val)
          (liftNumericLeaf candidate) =
        canAllocateLeaf (activeLeaves.val.map liftNumericLeaf) (liftNumericLeaf candidate))
  · rintro index ⟨bound, invariant⟩
    unfold path_scheme.can_allocate_leaf_loop.body
    dsimp only
    split
    · rename_i inRange
      have inList : index.val < activeLeaves.length := by scalar_tac
      step as ⟨element, elementIsAt⟩
      have sameSpec := same_leaf_id_refines element candidate
      step as ⟨matched, matchedIs⟩
      have dropped := drop_cons_getElem (activeLeaves.val.map liftNumericLeaf) index.val
        (by simpa using inList)
      rw [dropped] at invariant
      simp only [List.getElem_map] at invariant
      rw [← elementIsAt] at invariant
      split
      · rename_i isMatch
        simp only [WP.spec_ok]
        have eqLeaf : liftNumericLeaf element = liftNumericLeaf candidate := by
          exact of_decide_eq_true (matchedIs.symm.trans isMatch)
        rw [← invariant]
        simp [canAllocateLeaf, eqLeaf]
      · rename_i notMatch
        step as ⟨next, nextIs⟩
        refine ⟨by scalar_tac, ?_, by scalar_tac⟩
        have neLeaf : liftNumericLeaf element ≠ liftNumericLeaf candidate := by
          intro eqLeaf
          exact notMatch (matchedIs.trans (decide_eq_true eqLeaf))
        have neSymm : liftNumericLeaf candidate ≠ liftNumericLeaf element := Ne.symm neLeaf
        rw [nextIs, ← invariant]
        simp [canAllocateLeaf, neSymm]
    · rename_i outOfRange
      have atEnd : activeLeaves.length ≤ index.val := by scalar_tac
      simp only [WP.spec_ok]
      rw [← invariant, List.drop_eq_nil_of_le (by simpa using atEnd)]
      simp [canAllocateLeaf]
  · simp [canAllocateLeaf]

/-! ## New worktree setup and shared build cache -/

theorem default_settings_refines :
    setup.default_settings ⦃ settings =>
      liftWorktreeSettings settings = defaultSettings ⦄ := by
  simp [setup.default_settings, liftWorktreeSettings, defaultSettings]

theorem resolve_cargo_target_dir_refines (shareBuildCache : Bool) (sharedTargetDir : Path) :
    setup.resolve_cargo_target_dir shareBuildCache sharedTargetDir ⦃ resolved =>
      resolved = resolveCargoTargetDir shareBuildCache sharedTargetDir ⦄ := by
  cases shareBuildCache <;> simp [setup.resolve_cargo_target_dir, resolveCargoTargetDir]

theorem can_start_agent_refines (phase : setup.SetupPhase) :
    setup.can_start_agent phase ⦃ ready =>
      ready = canStartAgent (liftSetupPhase phase) ⦄ := by
  cases phase <;> simp [setup.can_start_agent, canStartAgent, liftSetupPhase]

theorem plan_setup_actions_refines (copyInst : core.marker.Copy Path)
    (settings : setup.WorktreeSettings) (worktreePath sharedTargetDir : Path) :
    setup.plan_setup_actions copyInst settings worktreePath sharedTargetDir ⦃ actions =>
      actions.val.map liftSetupAction =
        planSetupActions (liftWorktreeSettings settings) worktreePath sharedTargetDir ⦄ := by
  unfold setup.plan_setup_actions
  cases share : settings.share_build_cache <;>
    cases copy : settings.copy_untracked_files <;>
    cases runCmd : settings.run_setup_command <;>
    simp [alloc.vec.FromVecArray.from, planSetupActions, liftWorktreeSettings,
      liftSetupAction, share, copy, runCmd]

/-! ## Worktree lifecycle and guards -/

theorem new_record_refines (copyPath : core.marker.Copy Path)
    (copyBranch : core.marker.Copy Branch) (path : Path) (branch : Branch) :
    lifecycle.WorktreeRecord.new copyPath copyBranch path branch ⦃ record =>
      liftWorktreeRecord record = WorktreeRecord.new path branch ⦄ := by
  simp [lifecycle.WorktreeRecord.new, liftWorktreeRecord, liftLifecycleState, WorktreeRecord.new]

theorem step_lifecycle_refines (copyPath : core.marker.Copy Path)
    (copyBranch : core.marker.Copy Branch)
    (record : lifecycle.WorktreeRecord Path Branch)
    (event : lifecycle.LifecycleEvent Path) :
    lifecycle.WorktreeRecord.step copyPath copyBranch record event ⦃ commands next =>
      commands.val.map liftGitCommand =
        (stepLifecycle (liftWorktreeRecord record) (liftLifecycleEvent event)).2 ∧
      liftWorktreeRecord next =
        (stepLifecycle (liftWorktreeRecord record) (liftLifecycleEvent event)).1 ⦄ := by
  unfold lifecycle.WorktreeRecord.step
  cases event with
  | PrMerged deleteLocalBranch =>
    cases state : record.state <;> cases deleteLocalBranch <;>
      simp [alloc.vec.FromVecArray.from, stepLifecycle, liftWorktreeRecord,
        liftLifecycleState, liftLifecycleEvent, liftGitCommand, state]
  | PrClosedWithoutMerge =>
    cases state : record.state <;>
      simp [stepLifecycle, liftWorktreeRecord, liftLifecycleState,
        liftLifecycleEvent, state]
  | ThreadFailed =>
    cases state : record.state <;>
      simp [stepLifecycle, liftWorktreeRecord, liftLifecycleState,
        liftLifecycleEvent, state]
  | UserDeletedThread =>
    cases state : record.state <;>
      simp [alloc.vec.FromVecArray.from, stepLifecycle, liftWorktreeRecord,
        liftLifecycleState, liftLifecycleEvent, liftGitCommand, state]
  | LocationChanged newRoot =>
    simp [stepLifecycle, liftWorktreeRecord, liftLifecycleState,
      liftLifecycleEvent]

theorem prune_orphans_command_refines :
    lifecycle.prune_orphans_command Path Branch ⦃ command =>
      liftGitCommand command = pruneOrphansCommand ⦄ := by
  simp [lifecycle.prune_orphans_command, liftGitCommand, pruneOrphansCommand]

/-- Drives `WorktreeRecord::step` through a sequence of lifecycle events in turn. -/
@[expose] def runLifecycleGenerated (copyPath : core.marker.Copy Path)
    (copyBranch : core.marker.Copy Branch)
    (record : lifecycle.WorktreeRecord Path Branch) :
    List (lifecycle.LifecycleEvent Path) →
      Result (List (lifecycle.GitCommand Path Branch) × lifecycle.WorktreeRecord Path Branch)
  | [] => ok ([], record)
  | event :: rest => do
      let (commands, next) ← lifecycle.WorktreeRecord.step copyPath copyBranch record event
      let (laterCommands, final) ← runLifecycleGenerated copyPath copyBranch next rest
      ok (commands.val ++ laterCommands, final)

/-- The Rust worktree lifecycle, driven through any sequence of events, emits the exact
sequence of git commands and ends in the exact state specified by `runLifecycle` in
`Design.lean`. Therefore `correct`, `cost_bound`, and `optimal` hold of the Rust code. -/
theorem run_lifecycle_refines (copyPath : core.marker.Copy Path)
    (copyBranch : core.marker.Copy Branch)
    (record : lifecycle.WorktreeRecord Path Branch)
    (events : List (lifecycle.LifecycleEvent Path)) :
    runLifecycleGenerated copyPath copyBranch record events ⦃ commands final =>
      commands.map liftGitCommand =
        (runLifecycle (liftWorktreeRecord record) (events.map liftLifecycleEvent)).2 ∧
      liftWorktreeRecord final =
        (runLifecycle (liftWorktreeRecord record) (events.map liftLifecycleEvent)).1 ⦄ := by
  induction events generalizing record with
  | nil => simp [runLifecycleGenerated, runLifecycle]
  | cons event rest inductionHypothesis =>
    unfold runLifecycleGenerated
    have stepSpec := step_lifecycle_refines copyPath copyBranch record event
    step as ⟨commands, next, commandsAre, nextIs⟩
    have restSpec := inductionHypothesis next
    step as ⟨laterCommands, final, laterAre, finalIs⟩
    simp [runLifecycle, List.map_append, commandsAre, nextIs, laterAre, finalIs]

#print axioms effective_root_refines
#print axioms leaf_for_job_refines
#print axioms plan_worktree_path_refines
#print axioms same_leaf_id_refines
#print axioms can_allocate_leaf_refines
#print axioms default_settings_refines
#print axioms resolve_cargo_target_dir_refines
#print axioms can_start_agent_refines
#print axioms plan_setup_actions_refines
#print axioms new_record_refines
#print axioms step_lifecycle_refines
#print axioms prune_orphans_command_refines
#print axioms run_lifecycle_refines

end Velocity.WorktreeManager
