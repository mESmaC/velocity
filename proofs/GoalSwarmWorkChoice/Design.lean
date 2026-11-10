module
public import Mathlib.Data.List.Basic
public section

-- SPEC.md F16 (Goal swarm) and F17 (Proofs: goal swarm work choice).
-- Agents scan pool availability flags in order, attempt an atomic claim on the first
-- open piece, re-pick within one step on refusal, leave when nothing unclaimed remains,
-- and after merge either stop, start another round, or leave empty.
namespace GoalSwarmWorkChoice

inductive Decision where | claim (index : Nat) | leave deriving DecidableEq
inductive LockReply where | granted | refused deriving DecidableEq
inductive StepOutcome where | claimed (index : Nat) | retry (index : Nat) | left deriving DecidableEq
inductive PostMerge where | stop | continueSwarm | leaveEmpty deriving DecidableEq
inductive SessionResult where | claimed (index : Nat) | left deriving DecidableEq

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
  | [] => .left
  | false :: rest => shiftSession (runChoice (fun k => grants (k + 1)) rest)
  | true :: rest =>
      if grants 0 then .claimed 0 else shiftSession (runChoice (fun k => grants (k + 1)) rest)

def OpenAt (board : List Bool) (index : Nat) : Prop := board[index]? = some true

-- Independent specification of SPEC.md F16 work choice:
-- 1. Any claimed piece was open in the pool and granted by the channel lock.
-- 2. Every earlier open piece before a claimed piece lost a concurrent lock race.
-- 3. An agent leaves iff every open piece in the pool is refused by concurrent claims.
-- 4. Single-step choice, refusal retry, and post-merge `keep_going` match pool openness.
def Spec (grants : Nat -> Bool) (board : List Bool) (attempted : Nat) (keepGoing : Bool) : Prop :=
  (forall index, runChoice grants board = .claimed index ->
      OpenAt board index /\ grants index = true) /\
  (forall index prior, runChoice grants board = .claimed index ->
      prior < index -> OpenAt board prior -> grants prior = false) /\
  (runChoice grants board = .left <->
      forall index, OpenAt board index -> grants index = false) /\
  ((chooseOpen board = .leave <-> forall index, Not (OpenAt board index)) /\
    resolveAttempt attempted .granted board = .claimed attempted /\
    (resolveAttempt attempted .refused board = .left <-> chooseOpen board = .leave) /\
    (forall next, resolveAttempt attempted .refused board = .retry next -> OpenAt board next) /\
    (keepGoing = false -> decideAfterMerge keepGoing board = .stop) /\
    (decideAfterMerge true board = .leaveEmpty <-> chooseOpen board = .leave))

lemma chooseOpen_spec (board : List Bool) :
    (forall index, chooseOpen board = .claim index -> OpenAt board index) /\
    (chooseOpen board = .leave <-> forall index, Not (OpenAt board index)) := by
  induction board with
  | nil => simp [chooseOpen, OpenAt]
  | cons head rest ih =>
    cases head with
    | true =>
      refine ⟨fun _ eq => by cases eq; rfl, ?_⟩
      simp only [chooseOpen, reduceCtorEq, false_iff, Classical.not_forall]
      exact ⟨0, by simp [OpenAt]⟩
    | false =>
      refine ⟨fun index eq => ?_, fun eq pos => ?_, fun none_open => ?_⟩
      · cases chosen : chooseOpen rest <;> simp [chooseOpen, chosen, shiftDecision] at eq
        subst eq; exact ih.1 _ chosen
      · have lr : chooseOpen rest = .leave := by
          cases chosen : chooseOpen rest <;> simp [chooseOpen, chosen, shiftDecision] at eq; rfl
        cases pos with | zero => simp [OpenAt] | succ k => exact ih.2.mp lr k
      · simp [chooseOpen, ih.2.mpr fun k => none_open (k + 1), shiftDecision]

lemma runChoice_claimed (board : List Bool) (grants : Nat -> Bool) :
    forall index, runChoice grants board = .claimed index ->
      OpenAt board index /\ grants index = true /\
      forall prior, prior < index -> OpenAt board prior -> grants prior = false := by
  induction board generalizing grants with
  | nil => intro _ impossible; cases impossible
  | cons head rest ih =>
    intro index result
    cases head with
    | false =>
      cases sub : runChoice (fun k => grants (k + 1)) rest <;>
        simp [runChoice, sub, shiftSession] at result
      subst result; obtain ⟨is_open, won, prior_lost⟩ := ih (fun k => grants (k + 1)) _ sub
      refine ⟨is_open, won, fun prior lt open_prior => ?_⟩
      cases prior with
      | zero => simp [OpenAt] at open_prior | succ k => exact prior_lost k (by omega) open_prior
    | true =>
      simp only [runChoice] at result; split at result
      · rename_i won; cases result; exact ⟨rfl, won, fun _ lt _ => by omega⟩
      · rename_i lost
        cases sub : runChoice (fun k => grants (k + 1)) rest <;>
          simp [sub, shiftSession] at result
        subst result; obtain ⟨is_open, won, prior_lost⟩ := ih (fun k => grants (k + 1)) _ sub
        refine ⟨is_open, won, fun prior lt open_prior => ?_⟩
        cases prior with
        | zero => exact Bool.eq_false_iff.mpr lost | succ k => exact prior_lost k (by omega) open_prior

lemma runChoice_left (board : List Bool) (grants : Nat -> Bool) :
    runChoice grants board = .left <->
    forall index, OpenAt board index -> grants index = false := by
  induction board generalizing grants with
  | nil => simp [runChoice, OpenAt]
  | cons head rest ih =>
    have left_iff : shiftSession (runChoice (fun k => grants (k + 1)) rest) = .left <->
        runChoice (fun k => grants (k + 1)) rest = .left := by
      cases runChoice (fun k => grants (k + 1)) rest <;> simp [shiftSession]
    cases head with
    | false =>
      simp only [runChoice, left_iff, ih (fun k => grants (k + 1))]
      refine ⟨fun h pos hp => ?_, fun h k hk => h (k + 1) hk⟩
      cases pos with | zero => simp [OpenAt] at hp | succ k => exact h k hp
    | true =>
      simp only [runChoice]; split
      · rename_i won
        refine ⟨(by intro impossible; cases impossible), fun all_lost => ?_⟩
        have lost0 := all_lost 0 rfl; rw [won] at lost0; cases lost0
      · rename_i lost
        rw [left_iff, ih (fun k => grants (k + 1))]
        refine ⟨fun h pos hp => ?_, fun h k hk => h (k + 1) hk⟩
        cases pos with | zero => exact Bool.eq_false_iff.mpr lost | succ k => exact h k hp

theorem correct (grants : Nat -> Bool) (board : List Bool) (attempted : Nat) (keepGoing : Bool) :
    Spec grants board attempted keepGoing := by
  refine ⟨fun i h => ⟨(runChoice_claimed board grants i h).1, (runChoice_claimed board grants i h).2.1⟩,
          fun i p h => (runChoice_claimed board grants i h).2.2 p,
          runChoice_left board grants, ?_⟩
  obtain ⟨claim_open, leave_iff⟩ := chooseOpen_spec board
  refine ⟨leave_iff, rfl, ?_, ?_, (by intro off; subst off; rfl), ?_⟩
  · simp only [resolveAttempt]; cases chooseOpen board <;> simp
  · intro next eq
    cases chosen : chooseOpen board <;> simp [resolveAttempt, chosen] at eq
    subst eq; exact claim_open _ chosen
  · simp only [decideAfterMerge]; cases chooseOpen board <;> simp

-- Cost model: unit-cost `airc_claim` lock transactions issued by the choosing agent.
def attempts (grants : Nat -> Bool) : List Bool -> List Nat
  | [] => []
  | false :: rest => (attempts (fun k => grants (k + 1)) rest).map (· + 1)
  | true :: rest =>
      0 :: if grants 0 then [] else (attempts (fun k => grants (k + 1)) rest).map (· + 1)

def openCount : List Bool -> Nat
  | [] => 0 | true :: rest => 1 + openCount rest | false :: rest => openCount rest

def refusedCount (grants : Nat -> Bool) (board : List Bool) : Nat :=
  ((attempts grants board).filter (fun index => !grants index)).length

def cost (grants : Nat -> Bool) (board : List Bool) : Nat := (attempts grants board).length

theorem cost_bound (grants : Nat -> Bool) (board : List Bool) :
    cost grants board <= openCount board /\
    cost grants board <= refusedCount grants board + 1 := by
  induction board generalizing grants with
  | nil => simp [cost, attempts, openCount, refusedCount]
  | cons head rest ih =>
    obtain ⟨le_open, le_refused⟩ := ih (fun k => grants (k + 1))
    simp only [cost, refusedCount] at le_open le_refused
    cases head with
    | false =>
      simp only [cost, attempts, openCount, refusedCount, List.length_map, List.filter_map,
        Function.comp_def]
      exact ⟨le_open, le_refused⟩
    | true =>
      simp only [cost, attempts, openCount, refusedCount]; split
      · rename_i won; simp [won]
      · rename_i lost
        simp only [List.length_cons, List.length_map, List.filter_cons, lost, Bool.not_false,
          ↓reduceIte, List.filter_map, Function.comp_def]
        exact ⟨by omega, by omega⟩

-- Alternatives: priority-respecting lock-verified strategies with no hidden contention
-- oracle. Claiming `index` locks `index` and probes every earlier open slot before
-- skipping it; leaving empty probes every open slot. Extra probes are allowed at unit cost.
def RequiredProbes (board : List Bool) (result : SessionResult) (trace : List Nat) : Prop :=
  match result with
  | .claimed index =>
      index ∈ trace /\ forall prior, prior < index -> OpenAt board prior -> prior ∈ trace
  | .left => forall index, OpenAt board index -> index ∈ trace

def Admissible (grants : Nat -> Bool) (board : List Bool)
    (result : SessionResult) (trace : List Nat) : Prop :=
  result = runChoice grants board /\ RequiredProbes board result trace

lemma attempts_nodup (board : List Bool) (grants : Nat -> Bool) :
    (attempts grants board).Nodup := by
  induction board generalizing grants with
  | nil => simp [attempts]
  | cons head rest ih =>
    have mapped : ((attempts (fun k => grants (k + 1)) rest).map (· + 1)).Nodup :=
      List.Pairwise.map (· + 1) (fun _ _ ne => by omega) (ih (fun k => grants (k + 1)))
    cases head with
    | false => exact mapped
    | true =>
      simp only [attempts]; split
      · simp
      · exact List.nodup_cons.mpr ⟨by simp, mapped⟩

lemma attempts_in_required (board : List Bool) (grants : Nat -> Bool) :
    forall item, item ∈ attempts grants board ->
      OpenAt board item /\
      match runChoice grants board with | .claimed index => item <= index | .left => True := by
  induction board generalizing grants with
  | nil => intro _ mem; cases mem
  | cons head rest ih =>
    intro item mem
    cases head with
    | false =>
      simp only [attempts, List.mem_map] at mem; rcases mem with ⟨k, k_mem, rfl⟩
      obtain ⟨k_open, k_bound⟩ := ih (fun k => grants (k + 1)) k k_mem
      refine ⟨k_open, ?_⟩
      cases sub : runChoice (fun k => grants (k + 1)) rest <;>
        simp [runChoice, sub, shiftSession] at k_bound ⊢
      omega
    | true =>
      simp only [attempts, runChoice] at mem ⊢; split at mem <;> rename_i status
      · simp only [List.mem_singleton] at mem; subst mem; simp [status, OpenAt]
      · simp only [status, List.mem_cons, List.mem_map] at mem ⊢
        rcases mem with rfl | ⟨k, k_mem, rfl⟩
        · refine ⟨rfl, ?_⟩
          cases sub : runChoice (fun k => grants (k + 1)) rest <;> simp [shiftSession]
        · obtain ⟨k_open, k_bound⟩ := ih (fun k => grants (k + 1)) k k_mem
          refine ⟨k_open, ?_⟩
          cases sub : runChoice (fun k => grants (k + 1)) rest <;>
            simp [sub, shiftSession] at k_bound ⊢
          omega

lemma nodup_length_le (xs ys : List Nat)
    (nodup : xs.Nodup) (subset : forall item, item ∈ xs -> item ∈ ys) :
    xs.length <= ys.length := by
  induction xs generalizing ys with
  | nil => simp
  | cons head rest ih =>
    obtain ⟨not_mem, nodup_rest⟩ := List.nodup_cons.mp nodup
    have head_in : head ∈ ys := subset head List.mem_cons_self
    have step : rest.length <= (ys.erase head).length := by
      apply ih (ys.erase head) nodup_rest; intro item item_in
      have ne_head : item ≠ head := by rintro rfl; exact not_mem item_in
      exact (List.mem_erase_of_ne ne_head).mpr (subset item (List.mem_cons_of_mem head item_in))
    have erased_len := List.length_erase_of_mem head_in
    have pos_len := List.length_pos_of_mem head_in
    simp only [List.length_cons]; omega

theorem optimal (grants : Nat -> Bool) (board : List Bool)
    (result : SessionResult) (trace : List Nat)
    (alternative : Admissible grants board result trace) :
    cost grants board <= trace.length := by
  rcases alternative with ⟨rfl, required⟩
  apply nodup_length_le (attempts grants board) trace (attempts_nodup board grants)
  intro item mem; obtain ⟨is_open, bound⟩ := attempts_in_required board grants item mem
  cases outcome : runChoice grants board with
  | left => simp only [RequiredProbes, outcome] at required; exact required item is_open
  | claimed index =>
    simp only [RequiredProbes, outcome] at required bound
    by_cases eq_idx : item = index
    · subst eq_idx; exact required.1
    · exact required.2 item (by omega) is_open

#print axioms correct
#print axioms cost_bound
#print axioms optimal

-- Compact decision-class fixture checked against `velocity_core::goal_swarm_choice`.
def encodeFlags (board : List Bool) : String :=
  if board.isEmpty then "-" else String.intercalate "," (board.map fun b => if b then "1" else "0")
def chooseRow (b : List Bool) : String :=
  let out := match chooseOpen b with | .claim i => s!"claim:{i}" | .leave => "leave"
  s!"choose\t{encodeFlags b}\t-\t{out}\n"
def attemptRow (idx : Nat) (r : LockReply) (b : List Bool) : String :=
  let tag := match r with | .granted => "granted" | .refused => "refused"
  let out := match resolveAttempt idx r b with
    | .claimed i => s!"claimed:{i}" | .retry i => s!"retry:{i}" | .left => "left"
  s!"attempt\t{encodeFlags b}\t{idx}:{tag}\t{out}\n"
def mergeRow (kg : Bool) (b : List Bool) : String :=
  let out := match decideAfterMerge kg b with
    | .stop => "stop" | .continueSwarm => "continue" | .leaveEmpty => "leave_empty"
  s!"merge\t{encodeFlags b}\t{kg}\t{out}\n"
def fixtures : String :=
  chooseRow [] ++ chooseRow [false, false] ++ chooseRow [true, false, true] ++
  chooseRow [false, true, true] ++ chooseRow [false, false, true] ++
  attemptRow 0 .granted [false, true] ++ attemptRow 0 .refused [false, true, false] ++
  attemptRow 1 .refused [false, false, false] ++ mergeRow false [true, false] ++
  mergeRow false [] ++ mergeRow true [false, true] ++ mergeRow true [false, false]
#eval IO.FS.writeFile "fixtures/goal-swarm-work-choice.tsv" fixtures
end GoalSwarmWorkChoice