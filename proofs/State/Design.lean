module
public import Mathlib.Data.List.Basic
public import Mathlib.Tactic
@[expose] public section

/-!
# Persisted State Design Proof (`SPEC.md` S2.1, S2.2, S3, S3.1, F3, F5, F8, F13, F15, S7 M1)

This module models Velocity's persisted state for:
- **Projects** (`ProjectRecord`, including spec lock status, threads, and tasks)
- **Threads** (`ThreadRecord`, with states from `SPEC.md` S2.1, agent activity from `SPEC.md` S2.2,
  preferred vs. effective speed from F15, and fixed optional worktree path from F5)
- **Tasks** (`TaskRecord`, with states `draft`, `todo`, `now`, `done` from F8 and spec-lock gating from F3)
- **Settings** (`Settings`, covering Worktrees, Providers, Agents, and Proofs from F13/F15)
- **Dirty-checked atomic persistence** (`dirtyCheckedSchedule`, writing snapshots only when the
  state revision changes, with cost model and optimality proof over all stepwise-durable schedules).
-/

namespace Velocity.State

/-- Thread states from `SPEC.md` S2.1.
    Rule 1: there is no "waiting for user" or approval-gate state. -/
inductive ThreadState where
  | draft
  | spec
  | working
  | review
  | error
  | closed
  | done
  | merged
  deriving DecidableEq, Repr

/-- Agent activity states from `SPEC.md` S2.2. -/
inductive AgentActivity where
  | thinking
  | executing
  | parked
  | stalled
  | atLimit
  | idle
  deriving DecidableEq, Repr

/-- Task List states from `SPEC.md` F8. -/
inductive TaskState where
  | draft
  | todo
  | now
  | done
  deriving DecidableEq, Repr

/-- Who added a task (`SPEC.md` F8). -/
inductive TaskAttribution where
  | spec
  | user
  | thread (threadId : Nat)
  deriving DecidableEq, Repr

/-- Supported OAuth providers (`SPEC.md` S3, F15). -/
inductive ProviderKind where
  | claude
  | codex
  | antigravity
  | kimi
  deriving DecidableEq, Repr

/-- Agent effort levels (`SPEC.md` F15). -/
inductive EffortLevel where
  | low
  | medium
  | high
  | max
  deriving DecidableEq, Repr

/-- Speed mode (`SPEC.md` F15). -/
inductive SpeedMode where
  | regular
  | fast
  deriving DecidableEq, Repr

/-- Project location kind (`SPEC.md` F2). -/
inductive ProjectLocationKind where
  | local
  | github
  deriving DecidableEq, Repr

/-- PR review stages (`SPEC.md` S2.1, F7). -/
inductive ReviewStage where
  | ci
  | review
  | fixing
  | approved
  deriving DecidableEq, Repr

/-- Worktree settings (`SPEC.md` F5, F13), generic over path representation `PathValue`. -/
structure WorktreeSettings (PathValue : Type) where
  locationRoot : Option PathValue
  deleteLocalBranchOnMerge : Bool
  shareRustBuildCache : Bool
  warmPoolCount : Nat
  deriving DecidableEq, Repr

/-- Provider settings (`SPEC.md` F13, F15), generic over model representation `ModelValue`.
    Contains no API key fields (`SPEC.md` S1, F13, F15: OAuth only). -/
structure ProviderSettings (ModelValue : Type) where
  defaultProvider : ProviderKind
  defaultModel : Option ModelValue
  defaultEffort : EffortLevel
  defaultSpeed : SpeedMode
  titleModel : ModelValue
  titleEffort : EffortLevel
  titleSpeed : SpeedMode
  deriving DecidableEq, Repr

/-- Agent runtime caps and timeouts (`SPEC.md` S2.2, F13). -/
structure AgentSettings where
  maxTurns : Option Nat
  maxDurationMinutes : Option Nat
  stallTimeoutMinutes : Nat
  claimReleaseGraceMinutes : Nat
  deriving DecidableEq, Repr

/-- Proof and stress settings (`SPEC.md` F13, F17). -/
structure ProofSettings where
  stressDiffInputCount : Nat
  deriving DecidableEq, Repr

/-- Complete persisted settings (`SPEC.md` F13), generic over `PathValue` and `ModelValue`. -/
structure Settings (PathValue ModelValue : Type) where
  worktrees : WorktreeSettings PathValue
  providers : ProviderSettings ModelValue
  agents : AgentSettings
  proofs : ProofSettings
  deriving DecidableEq, Repr

/-- Default settings matching `SPEC.md` S2.2, F5, F13, F15, F17, parameterized by the
    default title model (`GPT-6 Luna` in F15). -/
def defaultSettings (defaultTitleModel : ModelValue) : Settings PathValue ModelValue :=
  { worktrees :=
      { locationRoot := none
        deleteLocalBranchOnMerge := true
        shareRustBuildCache := true
        warmPoolCount := 4 }
    providers :=
      { defaultProvider := .claude
        defaultModel := none
        defaultEffort := .high
        defaultSpeed := .regular
        titleModel := defaultTitleModel
        titleEffort := .low
        titleSpeed := .fast }
    agents :=
      { maxTurns := none
        maxDurationMinutes := none
        stallTimeoutMinutes := 5
        claimReleaseGraceMinutes := 10 }
    proofs :=
      { stressDiffInputCount := 10000 } }

/-- A persisted Task List item (`SPEC.md` F8), generic over `TextValue`. -/
structure TaskRecord (TextValue : Type) where
  id : Nat
  text : TextValue
  taskState : TaskState
  addedBy : TaskAttribution
  assignedThreadId : Option Nat
  deriving DecidableEq, Repr

/-- A persisted Thread item (`SPEC.md` S2.1, S2.2, F4, F5, F15), generic over `PathValue` and `ModelValue`. -/
structure ThreadRecord (PathValue ModelValue : Type) where
  id : Nat
  threadState : ThreadState
  activity : AgentActivity
  provider : ProviderKind
  model : ModelValue
  modelSupportsFast : Bool
  effort : EffortLevel
  preferredSpeed : SpeedMode
  effectiveSpeed : SpeedMode
  worktreePath : Option PathValue
  pullRequestNumber : Option Nat
  reviewStage : Option ReviewStage
  additions : Nat
  deletions : Nat
  deriving DecidableEq, Repr

/-- A persisted Project item (`SPEC.md` F1, F2, F3, F8), generic over `TextValue`, `PathValue`, and `ModelValue`. -/
structure ProjectRecord (TextValue PathValue ModelValue : Type) where
  id : Nat
  locationKind : ProjectLocationKind
  specLocked : Bool
  activeThreadId : Option Nat
  mergedToday : Nat
  tasks : List (TaskRecord TextValue)
  threads : List (ThreadRecord PathValue ModelValue)
  deriving DecidableEq, Repr

/-- Root persisted state (`SPEC.md` S7 M1). -/
structure PersistedState (TextValue PathValue ModelValue : Type) where
  activeProjectIndex : Nat
  projects : List (ProjectRecord TextValue PathValue ModelValue)
  settings : Settings PathValue ModelValue
  deriving DecidableEq, Repr

/-- Default empty persisted state (`SPEC.md` S7 M1). -/
def defaultPersistedState (defaultTitleModel : ModelValue) :
    PersistedState TextValue PathValue ModelValue :=
  { activeProjectIndex := 0
    projects := []
    settings := defaultSettings defaultTitleModel }

/-- Before a project's spec is locked, every task is a draft (`SPEC.md` F3, F8). -/
def normalizeTaskState (specLocked : Bool) (taskState : TaskState) : TaskState :=
  if specLocked then taskState else .draft

/-- Normalize a task record under its project's `specLocked` status (`SPEC.md` F3, F8). -/
def normalizeTask (specLocked : Bool) (task : TaskRecord TextValue) : TaskRecord TextValue :=
  { task with taskState := normalizeTaskState specLocked task.taskState }

/-- Resolve the speed a thread actually runs at (`SPEC.md` F15):
    Fast is used only when `modelSupportsFast` is true; otherwise Regular is used,
    while `preferredSpeed` remains stored on the thread so switching back restores Fast. -/
def resolveEffectiveSpeed (modelSupportsFast : Bool) (preferred : SpeedMode) : SpeedMode :=
  match modelSupportsFast, preferred with
  | true, .fast => .fast
  | _, _ => .regular

/-- Normalize a thread record (`SPEC.md` F5, F15):
    - `effectiveSpeed` is resolved from `modelSupportsFast` and `preferredSpeed`
    - `worktreePath` is preserved (`none` stays `none` for spec-interview and merged threads,
      and `some path` stays fixed when settings change). -/
def normalizeThread (thread : ThreadRecord PathValue ModelValue) :
    ThreadRecord PathValue ModelValue :=
  { thread with
    effectiveSpeed := resolveEffectiveSpeed thread.modelSupportsFast thread.preferredSpeed }

/-- Switch a thread to a new model (`SPEC.md` F15):
    updates `model` and `modelSupportsFast`, recomputes `effectiveSpeed`, and preserves `preferredSpeed`
    and `worktreePath`. -/
def switchThreadModel
    (thread : ThreadRecord PathValue ModelValue)
    (nextModel : ModelValue)
    (nextSupportsFast : Bool) : ThreadRecord PathValue ModelValue :=
  { thread with
    model := nextModel
    modelSupportsFast := nextSupportsFast
    effectiveSpeed := resolveEffectiveSpeed nextSupportsFast thread.preferredSpeed }

/-- Normalize a project record (`SPEC.md` F3, F5, F8, F15). -/
def normalizeProject (project : ProjectRecord TextValue PathValue ModelValue) :
    ProjectRecord TextValue PathValue ModelValue :=
  { project with
    tasks := project.tasks.map (normalizeTask project.specLocked)
    threads := project.threads.map normalizeThread }

/-- Clamp `activeProjectIndex` to a valid index (or `0` when `projectCount == 0`). -/
def normalizeActiveProjectIndex (index : Nat) (projectCount : Nat) : Nat :=
  if index < projectCount then index else 0

/-- Normalize the complete persisted state on load or mutation. -/
def normalizeState (state : PersistedState TextValue PathValue ModelValue) :
    PersistedState TextValue PathValue ModelValue :=
  { state with
    activeProjectIndex := normalizeActiveProjectIndex state.activeProjectIndex state.projects.length
    projects := state.projects.map normalizeProject }

/-- Update persisted settings (`get_settings` / `set_settings` in `SPEC.md` S3.1, F5, F13, F15).
    Changing `settings.worktrees.locationRoot` never alters existing threads' `worktreePath` (F5). -/
def applySettingsUpdate
    (state : PersistedState TextValue PathValue ModelValue)
    (newSettings : Settings PathValue ModelValue) :
    PersistedState TextValue PathValue ModelValue :=
  normalizeState { state with settings := newSettings }

/-- Specification predicate for a valid persisted state (`SPEC.md` S2.1, F3, F5, F8, F13, F15):
    1. Every project whose spec is not locked has all tasks in `draft` (`SPEC.md` F3, F8).
    2. Every thread's `effectiveSpeed` equals `resolveEffectiveSpeed thread.modelSupportsFast thread.preferredSpeed`,
       while `preferredSpeed` is preserved (`SPEC.md` F15).
    3. `activeProjectIndex` is in bounds whenever `projects` is non-empty (`0` when empty).
    4. Updating `settings` (including `worktrees.locationRoot`) never alters existing threads' `worktreePath` (F5). -/
def StateSpec (state : PersistedState TextValue PathValue ModelValue) : Prop :=
  (∀ project ∈ state.projects,
    project.specLocked = false →
      ∀ task ∈ project.tasks, task.taskState = .draft) ∧
  (∀ project ∈ state.projects,
    ∀ thread ∈ project.threads,
      thread.effectiveSpeed =
        resolveEffectiveSpeed thread.modelSupportsFast thread.preferredSpeed) ∧
  (state.projects = [] → state.activeProjectIndex = 0) ∧
  (state.projects ≠ [] → state.activeProjectIndex < state.projects.length) ∧
  (∀ newSettings : Settings PathValue ModelValue,
    (applySettingsUpdate state newSettings).projects.map (fun p => p.threads.map (·.worktreePath)) =
      state.projects.map (fun p => p.threads.map (·.worktreePath)))

/-- Dirty-checked snapshot decision: write to disk iff `target != disk`. -/
def shouldWriteSnapshot (disk : Nat) (target : Nat) : Bool :=
  target != disk

/-- Next persisted revision after a step. -/
def nextPersistedRevision (disk : Nat) (target : Nat) (writePerformed : Bool) : Nat :=
  if writePerformed then target else disk

/-- Dirty-checked persistence schedule for a sequence of target revisions `targets` starting from `disk`. -/
def dirtyCheckedSchedule : Nat → List Nat → List Bool
  | _, [] => []
  | disk, target :: rest =>
      shouldWriteSnapshot disk target :: dirtyCheckedSchedule target rest

/-- Cost model: number of atomic disk writes (`true` decisions) in `schedule`. -/
def diskWrites : List Bool → Nat
  | [] => 0
  | true :: rest => 1 + diskWrites rest
  | false :: rest => diskWrites rest

/-- Stepwise durability specification for a persistence schedule `schedule : List Bool`:
    after every step `target`, the revision on disk equals `target`. -/
def DurableAtEachStep : Nat → List Nat → List Bool → Prop
  | _, [], [] => True
  | _, [], _ :: _ => False
  | _, _ :: _, [] => False
  | disk, target :: restTargets, writeNow :: restSchedule =>
      let nextDisk := nextPersistedRevision disk target writeNow
      nextDisk = target ∧ DurableAtEachStep nextDisk restTargets restSchedule

/-- Combined design specification for persisted state and dirty-checked persistence. -/
def Spec
    (state : PersistedState TextValue PathValue ModelValue)
    (disk : Nat)
    (targets : List Nat)
    (schedule : List Bool) : Prop :=
  StateSpec state ∧
  normalizeState state = state ∧
  DurableAtEachStep disk targets schedule

@[simp]
theorem normalizeTaskState_idempotent (specLocked : Bool) (taskState : TaskState) :
    normalizeTaskState specLocked (normalizeTaskState specLocked taskState) =
      normalizeTaskState specLocked taskState := by
  cases specLocked <;> rfl

@[simp]
theorem normalizeTask_idempotent (specLocked : Bool) (task : TaskRecord TextValue) :
    normalizeTask specLocked (normalizeTask specLocked task) = normalizeTask specLocked task := by
  cases task
  simp [normalizeTask]

@[simp]
theorem normalizeThread_idempotent (thread : ThreadRecord PathValue ModelValue) :
    normalizeThread (normalizeThread thread) = normalizeThread thread := by
  cases thread
  simp [normalizeThread]

/-- F15: Switching a thread set to Fast to a model without a fast mode runs at Regular,
    and switching back to a model with a fast mode restores Fast without losing `preferredSpeed`. -/
theorem switchThreadModel_preserves_preferred_speed
    (thread : ThreadRecord PathValue ModelValue)
    (slowModel fastModel : ModelValue) :
    let onSlow := switchThreadModel thread slowModel false
    let onFastAgain := switchThreadModel onSlow fastModel true
    onSlow.preferredSpeed = thread.preferredSpeed ∧
    onSlow.effectiveSpeed = .regular ∧
    onFastAgain.preferredSpeed = thread.preferredSpeed ∧
    onFastAgain.effectiveSpeed = resolveEffectiveSpeed true thread.preferredSpeed ∧
    onFastAgain.worktreePath = thread.worktreePath := by
  simp [switchThreadModel, resolveEffectiveSpeed]

@[simp]
theorem normalizeProject_idempotent (project : ProjectRecord TextValue PathValue ModelValue) :
    normalizeProject (normalizeProject project) = normalizeProject project := by
  cases project with
  | mk id locationKind specLocked activeThreadId mergedToday tasks threads =>
    simp only [normalizeProject, List.map_map]
    have hTasks : List.map (normalizeTask specLocked ∘ normalizeTask specLocked) tasks =
        List.map (normalizeTask specLocked) tasks := by
      induction tasks with
      | nil => rfl
      | cons head tail ih => simp [Function.comp, normalizeTask_idempotent, ih]
    have hThreads : List.map (normalizeThread ∘ normalizeThread) threads =
        List.map normalizeThread threads := by
      induction threads with
      | nil => rfl
      | cons head tail ih => simp [Function.comp, normalizeThread_idempotent, ih]
    rw [hTasks, hThreads]

private theorem normalizeActiveProjectIndex_idempotent (index : Nat) (count : Nat) :
    normalizeActiveProjectIndex (normalizeActiveProjectIndex index count) count =
      normalizeActiveProjectIndex index count := by
  simp only [normalizeActiveProjectIndex]
  split
  · simp
  · split <;> rfl

theorem normalizeState_idempotent (state : PersistedState TextValue PathValue ModelValue) :
    normalizeState (normalizeState state) = normalizeState state := by
  cases state with
  | mk activeProjectIndex projects settings =>
    simp only [normalizeState, List.length_map, normalizeActiveProjectIndex_idempotent, List.map_map]
    congr 1
    induction projects with
    | nil => rfl
    | cons head tail ih =>
      simp [Function.comp, normalizeProject_idempotent, ih]

theorem normalizeState_meets_stateSpec (state : PersistedState TextValue PathValue ModelValue) :
    StateSpec (normalizeState state) := by
  refine ⟨?_, ?_, ?_, ?_, ?_⟩
  · intro project hProject hUnlocked task hTask
    rcases List.mem_map.mp hProject with ⟨rawProject, _, rfl⟩
    have hRawUnlocked : rawProject.specLocked = false := by
      simpa [normalizeProject] using hUnlocked
    rcases List.mem_map.mp hTask with ⟨rawTask, _, rfl⟩
    simp [normalizeTask, normalizeTaskState, hRawUnlocked]
  · intro project hProject thread hThread
    rcases List.mem_map.mp hProject with ⟨rawProject, _, rfl⟩
    rcases List.mem_map.mp hThread with ⟨rawThread, _, rfl⟩
    simp [normalizeThread]
  · intro hEmpty
    simp only [normalizeState, List.map_eq_nil_iff] at hEmpty ⊢
    simp [normalizeActiveProjectIndex, hEmpty]
  · intro hNonEmpty
    simp only [normalizeState, ne_eq, List.map_eq_nil_iff, List.length_map] at hNonEmpty ⊢
    have hPos : 0 < state.projects.length := List.length_pos_of_ne_nil hNonEmpty
    simp only [normalizeActiveProjectIndex]
    split
    · assumption
    · exact hPos
  · intro newSettings
    simp [applySettingsUpdate, normalizeState, normalizeProject, normalizeThread, List.map_map, Function.comp]

theorem dirtyCheckedSchedule_durable (disk : Nat) (targets : List Nat) :
    DurableAtEachStep disk targets (dirtyCheckedSchedule disk targets) := by
  induction targets generalizing disk with
  | nil =>
    simp [dirtyCheckedSchedule, DurableAtEachStep]
  | cons target rest ih =>
    simp only [dirtyCheckedSchedule, DurableAtEachStep, nextPersistedRevision, shouldWriteSnapshot,
      bne_iff_ne, ne_eq]
    by_cases hEq : target = disk
    · subst hEq
      simp [ih]
    · simp [hEq, ih]

/-- **Design Theorem 1 (`correct`)**:
`normalizeState` and `dirtyCheckedSchedule` satisfy `Spec` on every input. -/
theorem correct
    (state : PersistedState TextValue PathValue ModelValue)
    (disk : Nat)
    (targets : List Nat) :
    Spec (normalizeState state) disk targets (dirtyCheckedSchedule disk targets) := by
  refine ⟨normalizeState_meets_stateSpec state, normalizeState_idempotent state, ?_⟩
  exact dirtyCheckedSchedule_durable disk targets

/-- **Design Theorem 2 (`cost_bound`)**:
Under the `diskWrites` cost model, `dirtyCheckedSchedule` performs at most one atomic disk
write per state transition step (`targets.length`). -/
theorem cost_bound (disk : Nat) (targets : List Nat) :
    diskWrites (dirtyCheckedSchedule disk targets) <= targets.length := by
  induction targets generalizing disk with
  | nil =>
    simp [dirtyCheckedSchedule, diskWrites]
  | cons target rest ih =>
    simp only [dirtyCheckedSchedule]
    have hRest := ih target
    cases shouldWriteSnapshot disk target <;> simp [diskWrites] <;> omega

/-- **Design Theorem 3 (`optimal`)**:
Across the class of all stepwise-durable persistence schedules (`schedule : List Bool`
satisfying `DurableAtEachStep disk targets schedule`), no schedule performs fewer atomic
disk writes (`diskWrites`) than `dirtyCheckedSchedule`. -/
theorem optimal (disk : Nat) (targets : List Nat) (schedule : List Bool)
    (hDurable : DurableAtEachStep disk targets schedule) :
    diskWrites (dirtyCheckedSchedule disk targets) <= diskWrites schedule := by
  induction targets generalizing disk schedule with
  | nil =>
    cases schedule with
    | nil => simp [dirtyCheckedSchedule, diskWrites]
    | cons _ _ => simp [DurableAtEachStep] at hDurable
  | cons target rest ih =>
    cases schedule with
    | nil => simp [DurableAtEachStep] at hDurable
    | cons writeNow restSchedule =>
      simp only [DurableAtEachStep, nextPersistedRevision] at hDurable
      rcases hDurable with ⟨hStepDisk, hRestDurable⟩
      have hSub : diskWrites (dirtyCheckedSchedule target rest) <= diskWrites restSchedule := by
        rw [hStepDisk] at hRestDurable
        exact ih target restSchedule hRestDurable
      simp only [dirtyCheckedSchedule, shouldWriteSnapshot]
      cases hNe : target != disk
      · cases writeNow <;> simp [diskWrites] <;> omega
      · have hNeProp : target ≠ disk := bne_iff_ne.mp hNe
        cases writeNow with
        | true =>
          simp [diskWrites]
          omega
        | false =>
          simp at hStepDisk
          exact False.elim (hNeProp hStepDisk.symm)

#print axioms correct
#print axioms cost_bound
#print axioms optimal

end Velocity.State
