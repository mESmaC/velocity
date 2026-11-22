module
public import Mathlib.Data.List.Basic
public section

-- SPEC.md F16 (Goal swarm) and F17 (Proofs: goal swarm work choice).
-- Agents project persisted Rosen work state and AIRC holds onto pool eligibility, scan
-- eligible slots in order, attempt an atomic claim on the first open piece, re-pick
-- within one step on refusal, leave when none remain, and handle post-merge `keep_going`.
namespace GoalSwarmWorkChoice

inductive WorkKind where | task | issue deriving DecidableEq
inductive Projection where | absent | claimed | inChange | landed | green | reopened deriving DecidableEq
inductive Decision where | claim (index : Nat) | leave deriving DecidableEq
inductive LockReply where | granted | refused deriving DecidableEq
inductive StepOutcome where | claimed (index : Nat) | retry (index : Nat) | left deriving DecidableEq
inductive PostMerge where | stop | continueSwarm | leaveEmpty deriving DecidableEq
inductive SessionResult where | claimed (index : Nat) | left deriving DecidableEq

structure WorkSlot where
  kind : WorkKind
  sourceOpen : Bool
  activeOwner : Bool
  aircHeld : Bool
  projection : Projection
  reopenedAfter : Bool := false
  updatedAfter : Bool := false
  deriving DecidableEq

def Projection.completed : Projection -> Bool
  | .landed | .green => true | .absent | .claimed | .inChange | .reopened => false
def superseded (s : WorkSlot) : Bool :=
  match s.kind, s.projection with
  | .issue, .landed => s.reopenedAfter
  | .issue, .green => s.reopenedAfter || s.updatedAfter
  | _, _ => false
def projectedOpen (s : WorkSlot) : Bool :=
  s.sourceOpen && !s.activeOwner && !s.aircHeld && !(s.projection.completed && !superseded s)
def projectedBoard (slots : List WorkSlot) : List Bool := slots.map projectedOpen
def shiftDecision : Decision -> Decision | .claim i => .claim (i + 1) | .leave => .leave
def shiftSession : SessionResult -> SessionResult | .claimed i => .claimed (i + 1) | .left => .left
def chooseOpen : List Bool -> Decision
  | [] => .leave | true :: _ => .claim 0 | false :: rest => shiftDecision (chooseOpen rest)
def resolveAttempt (attempted : Nat) (reply : LockReply) (refreshed : List Bool) : StepOutcome :=
  match reply with
  | .granted => .claimed attempted
  | .refused => match chooseOpen refreshed with | .claim next => .retry next | .leave => .left
def decideAfterMerge (keepGoing : Bool) (refreshed : List Bool) : PostMerge :=
  if !keepGoing then .stop
  else match chooseOpen refreshed with | .claim _ => .continueSwarm | .leave => .leaveEmpty
def runChoice (grants : Nat -> Bool) : List Bool -> SessionResult
  | [] => .left | false :: rest => shiftSession (runChoice (fun k => grants (k + 1)) rest)
  | true :: rest => if grants 0 then .claimed 0 else shiftSession (runChoice (fun k => grants (k + 1)) rest)
def chooseProjected (slots : List WorkSlot) : Decision := chooseOpen (projectedBoard slots)
def resolveProjected (a : Nat) (r : LockReply) (s : List WorkSlot) : StepOutcome := resolveAttempt a r (projectedBoard s)
def decideAfterMergeProjected (kg : Bool) (s : List WorkSlot) : PostMerge := decideAfterMerge kg (projectedBoard s)
def runProjected (grants : Nat -> Bool) (slots : List WorkSlot) : SessionResult := runChoice grants (projectedBoard slots)
def OpenAt (board : List Bool) (index : Nat) : Prop := board[index]? = some true

-- Independent F16/F8 eligibility specification for a Rosen-projected work slot:
-- detached Landed/Green tasks stay excluded; released or reverted/reopened items stay
-- eligible; Landed issues require an explicit reopen; Green issues allow reopen or update.
def EligibleSlot (s : WorkSlot) : Prop :=
  s.sourceOpen = true /\ s.activeOwner = false /\ s.aircHeld = false /\
  (s.projection = .landed -> s.kind = .issue /\ s.reopenedAfter = true) /\
  (s.projection = .green -> s.kind = .issue /\ (s.reopenedAfter = true \/ s.updatedAfter = true))

def OpenProjected (slots : List WorkSlot) (index : Nat) : Prop :=
  exists slot, slots[index]? = some slot /\ EligibleSlot slot

-- A branchless AIRC claim excludes its work-item key from the F16 board (#531).
def unreserved (key : Nat) (savedOwner : Bool) (heldKeys : List Nat) : Bool :=
  !savedOwner && !heldKeys.contains key
def Spec (grants : Nat -> Bool) (slots : List WorkSlot) (attempted : Nat) (keepGoing : Bool) : Prop :=
  (forall key owner held, unreserved key owner held = true <-> owner = false /\ key ∉ held) /\
  (forall s, projectedOpen s = true <-> EligibleSlot s) /\
  (forall index, runProjected grants slots = .claimed index -> OpenProjected slots index /\ grants index = true) /\
  (forall index prior, runProjected grants slots = .claimed index ->
      prior < index -> OpenProjected slots prior -> grants prior = false) /\
  (runProjected grants slots = .left <-> forall index, OpenProjected slots index -> grants index = false) /\
  ((chooseProjected slots = .leave <-> forall index, Not (OpenProjected slots index)) /\
    resolveProjected attempted .granted slots = .claimed attempted /\
    (resolveProjected attempted .refused slots = .left <-> chooseProjected slots = .leave) /\
    (forall next, resolveProjected attempted .refused slots = .retry next -> OpenProjected slots next) /\
    (keepGoing = false -> decideAfterMergeProjected keepGoing slots = .stop) /\
    (decideAfterMergeProjected true slots = .leaveEmpty <-> chooseProjected slots = .leave))

lemma projectedOpen_iff (s : WorkSlot) : projectedOpen s = true <-> EligibleSlot s := by
  cases s with | mk kind src own held proj reop upd =>
    cases proj <;> cases kind <;> simp [projectedOpen, Projection.completed, superseded, EligibleSlot, and_assoc]
lemma openAt_projected (slots : List WorkSlot) (index : Nat) :
    OpenAt (projectedBoard slots) index <-> OpenProjected slots index := by
  simp only [OpenAt, projectedBoard, OpenProjected, List.getElem?_map]
  cases slots[index]? with | none => simp | some slot => simp [projectedOpen_iff]

lemma chooseOpen_spec (board : List Bool) :
    (forall index, chooseOpen board = .claim index -> OpenAt board index) /\
    (chooseOpen board = .leave <-> forall index, Not (OpenAt board index)) := by
  induction board with
  | nil => simp [chooseOpen, OpenAt]
  | cons head rest ih =>
    cases head with
    | true =>
      refine ⟨fun _ eq => by cases eq; rfl, ?_⟩
      simp only [chooseOpen, reduceCtorEq, false_iff, Classical.not_forall]; exact ⟨0, by simp [OpenAt]⟩
    | false =>
      refine ⟨fun index eq => ?_, fun eq pos => ?_, fun none_open => ?_⟩
      · cases c : chooseOpen rest <;> simp [chooseOpen, c, shiftDecision] at eq; subst eq; exact ih.1 _ c
      · have lr : chooseOpen rest = .leave := by cases c : chooseOpen rest <;> simp [chooseOpen, c, shiftDecision] at eq; rfl
        cases pos with | zero => simp [OpenAt] | succ k => exact ih.2.mp lr k
      · simp [chooseOpen, ih.2.mpr fun k => none_open (k + 1), shiftDecision]

lemma runChoice_claimed (board : List Bool) (grants : Nat -> Bool) :
    forall index, runChoice grants board = .claimed index ->
      OpenAt board index /\ grants index = true /\
      forall prior, prior < index -> OpenAt board prior -> grants prior = false := by
  induction board generalizing grants with
  | nil => intro _ h; cases h
  | cons head rest ih =>
    intro index result; cases head with
    | false =>
      cases sub : runChoice (fun k => grants (k + 1)) rest <;> simp [runChoice, sub, shiftSession] at result
      subst result; obtain ⟨o, w, p⟩ := ih (fun k => grants (k + 1)) _ sub
      exact ⟨o, w, fun prior lt op => by cases prior with | zero => simp [OpenAt] at op | succ k => exact p k (by omega) op⟩
    | true =>
      simp only [runChoice] at result; split at result
      · rename_i won; cases result; exact ⟨rfl, won, fun _ lt _ => by omega⟩
      · rename_i lost; cases sub : runChoice (fun k => grants (k + 1)) rest <;> simp [sub, shiftSession] at result
        subst result; obtain ⟨o, w, p⟩ := ih (fun k => grants (k + 1)) _ sub
        exact ⟨o, w, fun prior lt op => by cases prior with
          | zero => exact Bool.eq_false_iff.mpr lost | succ k => exact p k (by omega) op⟩

lemma runChoice_left (board : List Bool) (grants : Nat -> Bool) :
    runChoice grants board = .left <-> forall index, OpenAt board index -> grants index = false := by
  induction board generalizing grants with
  | nil => simp [runChoice, OpenAt]
  | cons head rest ih =>
    have left_iff : shiftSession (runChoice (fun k => grants (k + 1)) rest) = .left <->
        runChoice (fun k => grants (k + 1)) rest = .left := by cases runChoice (fun k => grants (k + 1)) rest <;> simp [shiftSession]
    cases head with
    | false =>
      simp only [runChoice, left_iff, ih (fun k => grants (k + 1))]
      exact ⟨fun h pos hp => by cases pos with | zero => simp [OpenAt] at hp | succ k => exact h k hp, fun h k hk => h (k + 1) hk⟩
    | true =>
      simp only [runChoice]; split
      · rename_i won; exact ⟨(by intro h; cases h), fun all_lost => by have h0 := all_lost 0 rfl; rw [won] at h0; cases h0⟩
      · rename_i lost; rw [left_iff, ih (fun k => grants (k + 1))]
        exact ⟨fun h pos hp => by cases pos with | zero => exact Bool.eq_false_iff.mpr lost | succ k => exact h k hp, fun h k hk => h (k + 1) hk⟩

theorem correct (grants : Nat -> Bool) (slots : List WorkSlot) (attempted : Nat) (keepGoing : Bool) :
    Spec grants slots attempted keepGoing := by
  have open_iff := openAt_projected slots; have claimed := runChoice_claimed (projectedBoard slots) grants
  have left_iff := runChoice_left (projectedBoard slots) grants; obtain ⟨claim_open, leave_iff⟩ := chooseOpen_spec (projectedBoard slots)
  refine ⟨(by intro key owner held; cases owner <;> simp [unreserved]), projectedOpen_iff, fun i h => ⟨(open_iff i).mp (claimed i h).1, (claimed i h).2.1⟩,
          fun i p h lt op => (claimed i h).2.2 p lt ((open_iff p).mpr op), by simp only [runProjected, left_iff, open_iff],
          ⟨by simp only [chooseProjected, leave_iff, open_iff], rfl, ?_, ?_, (by intro off; subst off; rfl), ?_⟩⟩
  · simp only [resolveProjected, resolveAttempt, chooseProjected]; cases chooseOpen (projectedBoard slots) <;> simp
  · intro next eq; cases c : chooseOpen (projectedBoard slots) <;> simp [resolveProjected, resolveAttempt, c] at eq
    subst eq; exact (open_iff _).mp (claim_open _ c)
  · simp only [decideAfterMergeProjected, decideAfterMerge, chooseProjected]; cases chooseOpen (projectedBoard slots) <;> simp

-- Cost model: one unit per AIRC work-item lock attempt; launch bookkeeping is external.
def attempts (grants : Nat -> Bool) : List Bool -> List Nat
  | [] => [] | false :: rest => (attempts (fun k => grants (k + 1)) rest).map (· + 1)
  | true :: rest => 0 :: if grants 0 then [] else (attempts (fun k => grants (k + 1)) rest).map (· + 1)
def openCount : List Bool -> Nat | [] => 0 | true :: r => 1 + openCount r | false :: r => openCount r
def refusedCount (grants : Nat -> Bool) (board : List Bool) : Nat := ((attempts grants board).filter (fun i => !grants i)).length
def cost (grants : Nat -> Bool) (board : List Bool) : Nat := (attempts grants board).length
def workItemLockAttemptCost (grants : Nat -> Bool) (slots : List WorkSlot) : Nat := cost grants (projectedBoard slots)

lemma cost_bound_board (grants : Nat -> Bool) (board : List Bool) :
    cost grants board <= openCount board /\ cost grants board <= refusedCount grants board + 1 := by
  induction board generalizing grants with
  | nil => simp [cost, attempts, openCount, refusedCount]
  | cons head rest ih =>
    obtain ⟨le_open, le_refused⟩ := ih (fun k => grants (k + 1)); simp only [cost, refusedCount] at le_open le_refused
    cases head with
    | false => simp only [cost, attempts, openCount, refusedCount, List.length_map, List.filter_map, Function.comp_def]; exact ⟨le_open, le_refused⟩
    | true =>
      simp only [cost, attempts, openCount, refusedCount]; split
      · rename_i won; simp [won]
      · rename_i lost; simp only [List.length_cons, List.length_map, List.filter_cons, lost, Bool.not_false, ↓reduceIte, List.filter_map, Function.comp_def]; exact ⟨by omega, by omega⟩

theorem cost_bound (grants : Nat -> Bool) (slots : List WorkSlot) :
    workItemLockAttemptCost grants slots <= openCount (projectedBoard slots) /\
    workItemLockAttemptCost grants slots <= refusedCount grants (projectedBoard slots) + 1 :=
  cost_bound_board grants (projectedBoard slots)

-- Alternatives: priority-respecting lock-verified strategies with no hidden contention
-- oracle. Claiming `index` locks `index` and probes every earlier projected-eligible slot
-- before skipping it; leaving empty probes every projected-eligible slot.
def RequiredProbes (slots : List WorkSlot) (result : SessionResult) (trace : List Nat) : Prop :=
  match result with
  | .claimed index => index ∈ trace /\ forall prior, prior < index -> OpenProjected slots prior -> prior ∈ trace
  | .left => forall index, OpenProjected slots index -> index ∈ trace

def PriorityLockStrategy (grants : Nat -> Bool) (slots : List WorkSlot) (result : SessionResult) (trace : List Nat) : Prop :=
  result = runProjected grants slots /\ RequiredProbes slots result trace

lemma attempts_nodup (board : List Bool) (grants : Nat -> Bool) : (attempts grants board).Nodup := by
  induction board generalizing grants with
  | nil => simp [attempts]
  | cons head rest ih =>
    have mapped : ((attempts (fun k => grants (k + 1)) rest).map (· + 1)).Nodup :=
      List.Pairwise.map (· + 1) (fun _ _ ne => by omega) (ih (fun k => grants (k + 1)))
    cases head with
    | false => exact mapped
    | true => cases h : grants 0 <;> simp [attempts, h, List.nodup_cons, mapped]

lemma attempts_in_required (board : List Bool) (grants : Nat -> Bool) :
    forall item, item ∈ attempts grants board ->
      OpenAt board item /\ match runChoice grants board with | .claimed index => item <= index | .left => True := by
  induction board generalizing grants with
  | nil => intro _ mem; cases mem
  | cons head rest ih =>
    intro item mem; cases head with
    | false =>
      simp only [attempts, List.mem_map] at mem; rcases mem with ⟨k, k_mem, rfl⟩
      obtain ⟨k_open, k_bound⟩ := ih (fun k => grants (k + 1)) k k_mem; refine ⟨k_open, ?_⟩
      cases sub : runChoice (fun k => grants (k + 1)) rest <;> simp [runChoice, sub, shiftSession] at k_bound ⊢; omega
    | true =>
      simp only [attempts, runChoice] at mem ⊢; split at mem <;> rename_i status
      · simp only [List.mem_singleton] at mem; subst mem; simp [status, OpenAt]
      · simp only [status, List.mem_cons, List.mem_map] at mem ⊢; rcases mem with rfl | ⟨k, k_mem, rfl⟩
        · refine ⟨rfl, ?_⟩; cases sub : runChoice (fun k => grants (k + 1)) rest <;> simp [shiftSession]
        · obtain ⟨k_open, k_bound⟩ := ih (fun k => grants (k + 1)) k k_mem; refine ⟨k_open, ?_⟩
          cases sub : runChoice (fun k => grants (k + 1)) rest <;> simp [sub, shiftSession] at k_bound ⊢; omega

lemma nodup_length_le (xs ys : List Nat) (nodup : xs.Nodup) (subset : forall item, item ∈ xs -> item ∈ ys) :
    xs.length <= ys.length := by
  induction xs generalizing ys with
  | nil => simp
  | cons head rest ih =>
    obtain ⟨not_mem, nodup_rest⟩ := List.nodup_cons.mp nodup; have head_in : head ∈ ys := subset head List.mem_cons_self
    have step : rest.length <= (ys.erase head).length := ih (ys.erase head) nodup_rest fun item item_in =>
      (List.mem_erase_of_ne (by rintro rfl; exact not_mem item_in)).mpr (subset item (List.mem_cons_of_mem head item_in))
    have := List.length_erase_of_mem head_in; have := List.length_pos_of_mem head_in; simp only [List.length_cons]; omega

theorem optimal (grants : Nat -> Bool) (slots : List WorkSlot)
    (result : SessionResult) (trace : List Nat) (alternative : PriorityLockStrategy grants slots result trace) :
    workItemLockAttemptCost grants slots <= trace.length := by
  rcases alternative with ⟨rfl, required⟩
  apply nodup_length_le (attempts grants (projectedBoard slots)) trace (attempts_nodup (projectedBoard slots) grants)
  intro item mem; obtain ⟨is_open, bound⟩ := attempts_in_required (projectedBoard slots) grants item mem
  have proj_open := (openAt_projected slots item).mp is_open
  cases outcome : runChoice grants (projectedBoard slots) with
  | left => simp only [RequiredProbes, runProjected, outcome] at required; exact required item proj_open
  | claimed index =>
    simp only [RequiredProbes, runProjected, outcome] at required bound
    by_cases eq_idx : item = index
    · subst eq_idx; exact required.1
    · exact required.2 item (by omega) proj_open

#print axioms correct
#print axioms cost_bound
#print axioms optimal

-- Compact decision-class fixtures checked against `velocity_core::goal_swarm_choice`
-- and the persisted inventory/claim paths in `velocity_threads::work_pool`.
def bit (b : Bool) : String := if b then "1" else "0"
def encodeFlags (b : List Bool) : String := if b.isEmpty then "-" else String.intercalate "," (b.map bit)
def kindName : WorkKind -> String | .task => "task" | .issue => "issue"
def projName : Projection -> String
  | .absent => "absent" | .claimed => "claimed" | .inChange => "in_change" | .landed => "landed" | .green => "green" | .reopened => "reopened"
def encodeSlot (s : WorkSlot) : String :=
  s!"{kindName s.kind}:{bit s.sourceOpen}:{bit s.activeOwner}:{bit s.aircHeld}:{projName s.projection}:{bit s.reopenedAfter}:{bit s.updatedAfter}"
def chooseRow (b : List Bool) : String := s!"choose\t{encodeFlags b}\t-\t{match chooseOpen b with | .claim i => s!"claim:{i}" | .leave => "leave"}\n"
def attemptRow (i : Nat) (r : LockReply) (b : List Bool) : String :=
  s!"attempt\t{encodeFlags b}\t{i}:{match r with | .granted => "granted" | .refused => "refused"}\t{match resolveAttempt i r b with | .claimed k => s!"claimed:{k}" | .retry k => s!"retry:{k}" | .left => "left"}\n"
def mergeRow (kg : Bool) (b : List Bool) : String :=
  s!"merge\t{encodeFlags b}\t{kg}\t{match decideAfterMerge kg b with | .stop => "stop" | .continueSwarm => "continue" | .leaveEmpty => "leave_empty"}\n"
def projectedRow (s : WorkSlot) : String := s!"projected\t{encodeSlot s}\t-\t{projectedOpen s}\n"
def projectedChooseRow (slots : List WorkSlot) : String :=
  s!"projected_choose\t{String.intercalate ";" (slots.map encodeSlot)}\t{encodeFlags (projectedBoard slots)}\t{match chooseProjected slots with | .claim i => s!"claim:{i}" | .leave => "leave"}\n"
def s (k : WorkKind) (src own held : Bool) (p : Projection) (reop upd : Bool := false) : WorkSlot :=
  { kind := k, sourceOpen := src, activeOwner := own, aircHeld := held, projection := p, reopenedAfter := reop, updatedAfter := upd }
def projectedCases : List WorkSlot := [
  s .task true false false .absent, s .task true false false .landed, s .task true false false .green,
  s .task true false false .reopened, s .task true false false .claimed, s .task true true false .reopened,
  s .task true false true .absent, s .task false false false .absent, s .issue true false false .absent,
  s .issue true false false .landed false false, s .issue true false false .landed false true,
  s .issue true false false .landed true true, s .issue true false false .green false false,
  s .issue true false false .green false true, s .issue true false false .green true true,
  s .issue true false false .reopened, s .issue true true false .reopened]
def projectedPools : List (List WorkSlot) := [
  [s .task true false false .landed, s .task true false false .reopened, s .task true false false .absent],
  [s .task true false false .landed, s .task true false false .green, s .task true false false .absent],
  [s .task true false false .landed, s .task true true false .reopened, s .task true false true .absent],
  [s .issue true false false .landed false true, s .issue true false false .landed true true, s .issue true false false .absent],
  [s .task true false false .landed, s .issue true false false .landed true true, s .task true false false .absent]]
def identityRow (key : Nat) (owner : Bool) (held : Nat) : String :=
  s!"identity\t{key}\t{owner}:{held}\t{unreserved key owner [held]}\n"
def fixtures : String :=
  chooseRow [] ++ chooseRow [false, false] ++ chooseRow [true, false, true] ++ chooseRow [false, true, true] ++
  chooseRow [false, false, true] ++ attemptRow 0 .granted [false, true] ++ attemptRow 0 .refused [false, true, false] ++
  attemptRow 1 .refused [false, false, false] ++ mergeRow false [true, false] ++ mergeRow false [] ++
  mergeRow true [false, true] ++ mergeRow true [false, false] ++
  String.join (projectedCases.map projectedRow) ++ String.join (projectedPools.map projectedChooseRow) ++
  identityRow 1 false 1 ++ identityRow 2 false 1 ++ identityRow 2 true 1
#eval IO.FS.writeFile "fixtures/goal-swarm-work-choice.tsv" fixtures
end GoalSwarmWorkChoice
