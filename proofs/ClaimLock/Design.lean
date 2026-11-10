module
public import Mathlib.Data.List.Basic
public section

-- SPEC.md F14: histories are newest operation first. The channel's cross-process
-- lock serializes each read/decide/append transaction; disk durability is external.
namespace ClaimLock
abbrev Key := Nat
abbrev Member := Nat
abbrev State := Key -> Option Member

inductive Operation where
  | claim (key : Key) (member : Member)
  | release (key : Key) (member : Member)
  | leave (member : Member)
  | fail (member : Member)
  deriving DecidableEq

def empty : State := fun _ => none

def replace (state : State) (key : Key) (owner : Option Member) : State :=
  fun query => if query = key then owner else state query

def step (state : State) : Operation -> State
  | .claim key member => if state key = none then replace state key (some member) else state
  | .release key member => if state key = some member then replace state key none else state
  | .leave _ | .fail _ => state

def run : List Operation -> State
  | [] => empty
  | operation :: prior => step (run prior) operation

-- Independent history specification: retain every request in the current epoch.
-- An epoch ends only on a release by its first requester. Reads of the head never
-- replace that first request with a later one, including after leave or failure.
def epoch : List Operation -> Key -> List Member
  | [], _ => []
  | .claim key member :: prior, query =>
      if query = key then epoch prior query ++ [member] else epoch prior query
  | .release key member :: prior, query =>
      if query = key && (epoch prior query).head? = some member then [] else epoch prior query
  | .leave _ :: prior, query | .fail _ :: prior, query => epoch prior query

def Spec (history : List Operation) : Prop :=
  (forall key, run history key = (epoch history key).head?) /\
  (forall key first second, run history key = some first ->
      run history key = some second -> first = second) /\
  (forall member, run (.leave member :: history) = run history /\
      run (.fail member :: history) = run history) /\
  (forall key member, Not (run history key = some member) ->
      run (.release key member :: history) = run history)

lemma first_wins (history : List Operation) :
    forall key, run history key = (epoch history key).head? := by
  induction history with
  | nil => intro key; rfl
  | cons operation prior hypothesis =>
    intro query
    cases operation with
    | claim key member =>
      by_cases same : query = key
      . subst query
        cases requests : epoch prior key with
        | nil => simp_all [run, step, epoch, replace]
        | cons owner rest => simp_all [run, step, epoch]
      . by_cases free : run prior key = none
        . simp_all [run, step, epoch, replace]
        . simp_all [run, step, epoch]
    | release key member =>
      by_cases held : run prior key = some member
      . by_cases same : query = key
        . subst query; simp_all [run, step, epoch, replace]
        . simp_all [run, step, epoch, replace]
      . by_cases same : query = key
        . subst query; simp_all [run, step, epoch]
        . simp_all [run, step, epoch]
    | leave member => exact hypothesis query
    | fail member => exact hypothesis query

theorem correct (history : List Operation) : Spec history := by
  refine And.intro (first_wins history) ?_
  refine And.intro ?_ (And.intro ?_ ?_)
  . intro key first second left right
    exact Option.some.inj (left.symm.trans right)
  . intro member
    exact And.intro rfl rfl
  . intro key member notOwner
    simp [run, step, notOwner]

inductive Access where
  | read (key : Key)
  | write (key : Key)
  deriving DecidableEq

-- Unit-cost ownership-cell reads and writes, including unsuccessful decisions.
-- Log replay and file-overlap enumeration are outside this ownership-cell model.
-- Each trace is instrumented from the actual branch taken by the transition.
def accesses (state : State) : Operation -> List Access
  | .claim key _ => .read key :: if state key = none then [.write key] else []
  | .release key member => .read key :: if state key = some member then [.write key] else []
  | .leave _ | .fail _ => []

def cost (history : List Operation) : Nat :=
  match history with
  | [] => 0
  | operation :: prior => cost prior + (accesses (run prior) operation).length

lemma step_bound (state : State) (operation : Operation) :
    (accesses state operation).length <= 2 := by
  cases operation <;> simp only [accesses]
  all_goals first | (split <;> simp) | simp

theorem cost_bound (history : List Operation) : cost history <= 2 * history.length := by
  induction history with
  | nil => simp [cost]
  | cons operation prior hypothesis =>
    have bound := step_bound (run prior) operation
    simp only [cost, List.length_cons]
    omega

-- Alternatives: serialized ownership-cell strategies with no hidden ownership
-- oracle. Claims/releases inspect their target cell before deciding. A successful
-- acquisition or authorized release writes that cell. Additional accesses are
-- permitted, and each read/write has the same unit cost as above.
def RequiredAccesses (state : State) (operation : Operation) (trace : List Access) : Prop :=
  match operation with
  | .claim key _ => Access.read key ∈ trace /\
      (state key = none -> Access.write key ∈ trace)
  | .release key member => Access.read key ∈ trace /\
      (state key = some member -> Access.write key ∈ trace)
  | .leave _ | .fail _ => True

def Admissible (state : State) (operation : Operation)
    (result : State) (trace : List Access) : Prop :=
  result = step state operation /\ RequiredAccesses state operation trace

lemma two_accesses (trace : List Access) (key : Key)
    (read : Access.read key ∈ trace) (write : Access.write key ∈ trace) :
    2 <= trace.length := by
  cases trace with
  | nil => simp at read
  | cons first rest =>
    cases rest with
    | nil =>
      simp only [List.mem_singleton] at read write
      have impossible := read.trans write.symm
      cases impossible
    | cons second rest => simp

lemma optimal_claim (state : State) (key member : Nat) (trace : List Access)
    (valid : RequiredAccesses state (.claim key member) trace) :
    (accesses state (.claim key member)).length <= trace.length := by
  simp only [accesses]
  split
  . rename_i free
    exact two_accesses trace key valid.1 (valid.2 free)
  . exact List.length_pos_of_mem valid.1

lemma optimal_release (state : State) (key member : Nat) (trace : List Access)
    (valid : RequiredAccesses state (.release key member) trace) :
    (accesses state (.release key member)).length <= trace.length := by
  simp only [accesses]
  split
  . rename_i owner
    exact two_accesses trace key valid.1 (valid.2 owner)
  . exact List.length_pos_of_mem valid.1

theorem optimal (history : List Operation) (operation : Operation)
    (result : State) (trace : List Access)
    (alternative : Admissible (run history) operation result trace) :
    (accesses (run history) operation).length <= trace.length := by
  cases operation with
  | claim key member => exact optimal_claim _ key member trace alternative.2
  | release key member => exact optimal_release _ key member trace alternative.2
  | leave member => exact Nat.zero_le _
  | fail member => exact Nat.zero_le _

#print axioms correct
#print axioms cost_bound
#print axioms optimal

-- Decision classes retained as the Rust confidence fixture.
def allowed (holder : Option Member) (requester : Member) : Bool :=
  holder.isNone || holder == some requester

def fixtureLine (holder : Option Member) (requester : Member) : String :=
  let label := holder.map toString |>.getD "-"
  s!"{label}\t{requester}\t{allowed holder requester}\n"

def fixtures : String :=
  fixtureLine none 7 ++ fixtureLine (some 7) 7 ++ fixtureLine (some 8) 7
#eval IO.FS.writeFile "fixtures/claim-lock.tsv" fixtures
end ClaimLock
