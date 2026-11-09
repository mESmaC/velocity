module
public import State.Design
public import State.Generated.Types
public import State.Generated.Funs
public section

/-!
# Persisted State Confidence Proof (`SPEC.md` S2.1, S2.2, S3, S3.1, F3, F5, F8, F13, F15, S7 M1, F17)

Proof level: **code** (`Rust · code · Aeneas`).
`crates/velocity-core/src/state.rs` is extracted by Charon and translated to Lean 4 by Aeneas
(`State.Generated.Types` and `State.Generated.Funs`).
The theorems below prove that every translated Rust function terminates without failure and
refines the corresponding operation in `Velocity.State` (`State.Design`), polymorphic over
`TextValue`, `PathValue`, and `ModelValue` (covering the real `String` paths and model identifiers
persisted by Velocity).
-/

namespace Velocity.State

open Aeneas Aeneas.Std Result
open Velocity.State.Generated

@[expose] def toDesignThreadState : state.ThreadState -> ThreadState
  | .Draft => .draft
  | .Spec => .spec
  | .Working => .working
  | .Review => .review
  | .Error => .error
  | .Closed => .closed
  | .Done => .done
  | .Merged => .merged

@[expose] def toDesignAgentActivity : state.AgentActivity -> AgentActivity
  | .Thinking => .thinking
  | .Executing => .executing
  | .Parked => .parked
  | .Stalled => .stalled
  | .AtLimit => .atLimit
  | .Idle => .idle

@[expose] def toDesignTaskState : state.TaskState -> TaskState
  | .Draft => .draft
  | .Todo => .todo
  | .Now => .now
  | .Done => .done

@[expose] def toDesignTaskAttribution : state.TaskAttribution -> TaskAttribution
  | .Spec => .spec
  | .User => .user
  | .Thread tid => .thread tid.val

@[expose] def toDesignProviderKind : state.ProviderKind -> ProviderKind
  | .Claude => .claude
  | .Codex => .codex
  | .Antigravity => .antigravity
  | .Kimi => .kimi

@[expose] def toDesignEffortLevel : state.EffortLevel -> EffortLevel
  | .Low => .low
  | .Medium => .medium
  | .High => .high
  | .Max => .max

@[expose] def toDesignSpeedMode : state.SpeedMode -> SpeedMode
  | .Regular => .regular
  | .Fast => .fast

@[expose] def toDesignReviewStage : state.ReviewStage -> ReviewStage
  | .ContinuousIntegration => .ci
  | .Review => .review
  | .Fixing => .fixing
  | .Approved => .approved

@[expose] def toDesignOptionU32 : Option Std.U32 -> Option Nat
  | none => none
  | some value => some value.val

@[expose] def toDesignOptionReviewStage : Option state.ReviewStage -> Option ReviewStage
  | none => none
  | some stage => some (toDesignReviewStage stage)

@[expose] def toDesignWorktreeSettings
    (settings : state.WorktreeSettings PathValue) : WorktreeSettings PathValue :=
  { locationRoot := settings.location_root
    deleteLocalBranchOnMerge := settings.delete_local_branch_on_merge
    shareRustBuildCache := settings.share_rust_build_cache
    warmPoolCount := settings.warm_pool_count.val }

@[expose] def toDesignProviderSettings
    (settings : state.ProviderSettings ModelValue) : ProviderSettings ModelValue :=
  { defaultProvider := toDesignProviderKind settings.default_provider
    defaultModel := settings.default_model
    defaultEffort := toDesignEffortLevel settings.default_effort
    defaultSpeed := toDesignSpeedMode settings.default_speed
    titleModel := settings.title_model
    titleEffort := toDesignEffortLevel settings.title_effort
    titleSpeed := toDesignSpeedMode settings.title_speed }

@[expose] def toDesignAgentSettings (settings : state.AgentSettings) : AgentSettings :=
  { maxTurns := toDesignOptionU32 settings.max_turns
    maxDurationMinutes := toDesignOptionU32 settings.max_duration_minutes
    stallTimeoutMinutes := settings.stall_timeout_minutes.val
    claimReleaseGraceMinutes := settings.claim_release_grace_minutes.val }

@[expose] def toDesignProofSettings (settings : state.ProofSettings) : ProofSettings :=
  { stressDiffInputCount := settings.stress_diff_input_count.val }

@[expose] def toDesignSettings
    (settings : state.Settings PathValue ModelValue) : Settings PathValue ModelValue :=
  { worktrees := toDesignWorktreeSettings settings.worktrees
    providers := toDesignProviderSettings settings.providers
    agents := toDesignAgentSettings settings.agents
    proofs := toDesignProofSettings settings.proofs }

@[expose] def toDesignTaskRecord
    (task : state.TaskRecord TextValue) : TaskRecord TextValue :=
  { id := task.id.val
    text := task.text
    taskState := toDesignTaskState task.task_state
    addedBy := toDesignTaskAttribution task.added_by
    assignedThreadId := toDesignOptionU32 task.assigned_thread_id }

@[expose] def toDesignThreadRecord
    (thread : state.ThreadRecord PathValue ModelValue) : ThreadRecord PathValue ModelValue :=
  { id := thread.id.val
    threadState := toDesignThreadState thread.thread_state
    activity := toDesignAgentActivity thread.activity
    provider := toDesignProviderKind thread.provider
    model := thread.model
    modelSupportsFast := thread.model_supports_fast
    effort := toDesignEffortLevel thread.effort
    preferredSpeed := toDesignSpeedMode thread.preferred_speed
    effectiveSpeed := toDesignSpeedMode thread.effective_speed
    worktreePath := thread.worktree_path
    pullRequestNumber := toDesignOptionU32 thread.pull_request_number
    reviewStage := toDesignOptionReviewStage thread.review_stage
    additions := thread.additions.val
    deletions := thread.deletions.val }

theorem normalize_task_state_refines (specLocked : Bool) (taskState : state.TaskState) :
    ∃ normalized : state.TaskState,
      state.normalize_task_state specLocked taskState = .ok normalized /\
      toDesignTaskState normalized = normalizeTaskState specLocked (toDesignTaskState taskState) := by
  cases specLocked <;> cases taskState <;> exact ⟨_, And.intro rfl rfl⟩

theorem normalize_task_record_refines
    (specLocked : Bool) (task : state.TaskRecord TextValue) :
    ∃ normalized : state.TaskRecord TextValue,
      state.normalize_task_record specLocked task = .ok normalized /\
      toDesignTaskRecord normalized = normalizeTask specLocked (toDesignTaskRecord task) := by
  rcases normalize_task_state_refines specLocked task.task_state with ⟨ts, hCall, hVal⟩
  refine ⟨{ task with task_state := ts }, And.intro ?_ ?_⟩
  · simp [state.normalize_task_record, hCall]
  · simp [toDesignTaskRecord, normalizeTask, hVal]

theorem resolve_effective_speed_refines (modelSupportsFast : Bool) (preferred : state.SpeedMode) :
    ∃ resolved : state.SpeedMode,
      state.resolve_effective_speed modelSupportsFast preferred = .ok resolved /\
      toDesignSpeedMode resolved = resolveEffectiveSpeed modelSupportsFast (toDesignSpeedMode preferred) := by
  cases modelSupportsFast <;> cases preferred <;> exact ⟨_, And.intro rfl rfl⟩

theorem normalize_thread_record_refines
    (thread : state.ThreadRecord PathValue ModelValue) :
    ∃ normalized : state.ThreadRecord PathValue ModelValue,
      state.normalize_thread_record thread = .ok normalized /\
      toDesignThreadRecord normalized = normalizeThread (toDesignThreadRecord thread) /\
      normalized.worktree_path = thread.worktree_path := by
  rcases resolve_effective_speed_refines thread.model_supports_fast thread.preferred_speed with
    ⟨resolved, hCall, hVal⟩
  refine ⟨{ thread with effective_speed := resolved }, And.intro ?_ (And.intro ?_ rfl)⟩
  · simp [state.normalize_thread_record, hCall]
  · simp [toDesignThreadRecord, normalizeThread, hVal]

/-- Confidence proof covering the PR-duty stress evidence (stress/pr5-state-load):
    threads with no worktree (worktree_path = none, such as Spec and Merged threads)
    still have worktree_path = none after normalization. -/
theorem normalize_thread_record_preserves_no_worktree {PathValue ModelValue : Type}
    (thread : state.ThreadRecord PathValue ModelValue)
    (hNoWorktree : thread.worktree_path = none) :
    ∃ normalized : state.ThreadRecord PathValue ModelValue,
      state.normalize_thread_record thread = .ok normalized /\
      normalized.worktree_path = none := by
  rcases normalize_thread_record_refines thread with ⟨normalized, hCall, _, hPath⟩
  exact ⟨normalized, hCall, hPath.trans hNoWorktree⟩

theorem switch_thread_model_refines
    (thread : state.ThreadRecord PathValue ModelValue)
    (nextModel : ModelValue)
    (nextSupportsFast : Bool) :
    ∃ switched : state.ThreadRecord PathValue ModelValue,
      state.switch_thread_model thread nextModel nextSupportsFast = .ok switched /\
      toDesignThreadRecord switched =
        switchThreadModel (toDesignThreadRecord thread) nextModel nextSupportsFast /\
      switched.preferred_speed = thread.preferred_speed /\
      switched.worktree_path = thread.worktree_path := by
  rcases resolve_effective_speed_refines nextSupportsFast thread.preferred_speed with
    ⟨resolved, hCall, hVal⟩
  refine ⟨{ thread with model := nextModel, model_supports_fast := nextSupportsFast, effective_speed := resolved },
    And.intro ?_ (And.intro ?_ (And.intro rfl rfl))⟩
  · simp [state.switch_thread_model, hCall]
  · simp [toDesignThreadRecord, switchThreadModel, hVal]

theorem normalize_active_project_index_refines (index : Std.U32) (projectCount : Std.U32) :
    ∃ normalized : Std.U32,
      state.normalize_active_project_index index projectCount = .ok normalized /\
      normalized.val = normalizeActiveProjectIndex index.val projectCount.val := by
  unfold state.normalize_active_project_index normalizeActiveProjectIndex
  by_cases hLt : index < projectCount
  · have hValLt : index.val < projectCount.val := hLt
    simp [hLt, hValLt]
  · have hValNotLt : ¬ index.val < projectCount.val := hLt
    simp [hLt, hValNotLt]

theorem normalize_persisted_state_header_refines
    (header : state.PersistedStateHeader PathValue ModelValue) :
    ∃ normalized : state.PersistedStateHeader PathValue ModelValue,
      state.normalize_persisted_state_header header = .ok normalized /\
      normalized.active_project_index.val =
        normalizeActiveProjectIndex header.active_project_index.val header.project_count.val /\
      normalized.project_count = header.project_count /\
      toDesignSettings normalized.settings = toDesignSettings header.settings := by
  rcases normalize_active_project_index_refines header.active_project_index header.project_count with
    ⟨idx, hIdxCall, hIdxVal⟩
  refine ⟨{ header with active_project_index := idx }, And.intro ?_ (And.intro hIdxVal (And.intro rfl rfl))⟩
  simp [state.normalize_persisted_state_header, hIdxCall]

theorem apply_settings_update_refines
    (header : state.PersistedStateHeader PathValue ModelValue)
    (newSettings : state.Settings PathValue ModelValue) :
    ∃ updated : state.PersistedStateHeader PathValue ModelValue,
      state.apply_settings_update header newSettings = .ok updated /\
      updated.active_project_index.val =
        normalizeActiveProjectIndex header.active_project_index.val header.project_count.val /\
      toDesignSettings updated.settings = toDesignSettings newSettings /\
      updated.project_count = header.project_count := by
  rcases normalize_active_project_index_refines header.active_project_index header.project_count with
    ⟨idx, hIdxCall, hIdxVal⟩
  refine ⟨{ header with active_project_index := idx, settings := newSettings },
    And.intro ?_ (And.intro hIdxVal (And.intro rfl rfl))⟩
  simp [state.apply_settings_update, state.normalize_persisted_state_header, hIdxCall]

theorem default_settings_refines (defaultTitleModel : ModelValue) :
    ∃ settings : state.Settings PathValue ModelValue,
      state.default_settings PathValue defaultTitleModel = .ok settings /\
      toDesignSettings settings = defaultSettings defaultTitleModel := by
  refine ⟨_, And.intro rfl rfl⟩

theorem should_write_snapshot_refines (diskRevision : Std.U32) (nextRevision : Std.U32) :
    state.should_write_snapshot diskRevision nextRevision =
      .ok (shouldWriteSnapshot diskRevision.val nextRevision.val) := by
  rfl

theorem apply_dirty_checked_step_refines (diskRevision : Std.U32) (targetRevision : Std.U32) :
    ∃ writeNow : Bool, ∃ nextDisk : Std.U32,
      state.apply_dirty_checked_step diskRevision targetRevision = .ok (writeNow, nextDisk) /\
      writeNow = shouldWriteSnapshot diskRevision.val targetRevision.val /\
      nextDisk.val = targetRevision.val := by
  refine ⟨shouldWriteSnapshot diskRevision.val targetRevision.val, targetRevision, And.intro ?_ (And.intro rfl rfl)⟩
  unfold state.apply_dirty_checked_step
  rw [should_write_snapshot_refines]
  simp only [bind_ok, state.next_persisted_revision, shouldWriteSnapshot, bne_iff_ne, ne_eq]
  by_cases hEq : targetRevision = diskRevision
  · subst hEq
    simp
  · have hValNe : Not (targetRevision.val = diskRevision.val) := by
      intro hValEq
      apply hEq
      exact Std.UScalar.eq_of_val_eq hValEq
    simp [hValNe]

#print axioms normalize_task_state_refines
#print axioms normalize_task_record_refines
#print axioms resolve_effective_speed_refines
#print axioms normalize_thread_record_refines
#print axioms normalize_thread_record_preserves_no_worktree
#print axioms switch_thread_model_refines
#print axioms normalize_active_project_index_refines
#print axioms normalize_persisted_state_header_refines
#print axioms apply_settings_update_refines
#print axioms default_settings_refines
#print axioms should_write_snapshot_refines
#print axioms apply_dirty_checked_step_refines

end Velocity.State