module
public import Mathlib.Data.List.Basic
public section
-- SPEC.md §2.2 and F17: live claims are retained; continuously unavailable
-- claims are released at the first observed deadline, unless a PR protects them.
namespace AgentActivity
inductive Action where | nothing | nudge | restart deriving DecidableEq, Repr

def stall (silence timeout : Nat) (nudged exempt : Bool) : Action :=
  if exempt then .nothing else if silence < timeout then .nothing
  else if nudged then .restart else .nudge

inductive Status where
  | live | failed | atLimit | parked | buildQueue | closed
  deriving DecidableEq, Repr

structure Observation where
  time : Nat
  status : Status
  pullRequest : Bool := false
  deriving Repr

def unavailable (observation : Observation) : Bool :=
  observation.status == .failed || observation.status == .atLimit

def claimProtected (observation : Observation) : Bool :=
  observation.pullRequest || observation.status == .closed

structure Checkpoint where
  since : Option Nat := none
  released : Bool := false
  commands : Nat := 0
  deriving Repr

def checkpoint (previous : Option Nat) (observation : Observation) : Option Nat :=
  if unavailable observation then some (previous.getD observation.time) else none

def releaseDue (since : Option Nat) (observation : Observation) (grace : Nat) : Bool :=
  unavailable observation && !claimProtected observation &&
    since.any (fun start => decide (grace ≤ observation.time - start))

def tick (previous : Checkpoint) (observation : Observation) (grace : Nat) : Checkpoint :=
  let since := checkpoint previous.since observation
  let release := releaseDue since observation grace && !previous.released
  { since, released := previous.released || release,
    commands := previous.commands + if release then 1 else 0 }

-- Traces are newest first. The suffix ending now is broken by *any* available
-- observation (including ACP recovery, parking and waiting/running in the queue).
-- This specification has no caller-supplied eligibility Boolean or checkpoint.
def intervalStart : List Observation → Option Nat
  | [] => none
  | observation :: history =>
    if unavailable observation then
      some ((intervalStart history).getD observation.time) else none

def due (history : List Observation) (grace : Nat) : Bool :=
  match history with
  | [] => false
  | observation :: _ => releaseDue (intervalStart history) observation grace

-- An ownership epoch ends at release; reacquisition begins a fresh empty trace.
def everDue : List Observation → Nat → Bool
  | [], _ => false
  | observation :: history, grace => everDue history grace || due (observation :: history) grace

def run : List Observation → Nat → Checkpoint
  | [], _ => {}
  | observation :: history, grace => tick (run history grace) observation grace

lemma trace_invariant (history : List Observation) (grace : Nat) :
    (run history grace).since = intervalStart history ∧
    (run history grace).released = everDue history grace ∧
    (run history grace).commands = if everDue history grace then 1 else 0 := by
  induction history with
  | nil => simp [run, intervalStart, everDue]
  | cons observation history induction =>
    rcases induction with ⟨since, released, commands⟩
    simp only [run, tick, since, released, commands]
    change checkpoint (intervalStart history) observation = intervalStart (observation :: history) ∧
      (everDue history grace || (due (observation :: history) grace && !everDue history grace)) =
        (everDue history grace || due (observation :: history) grace) ∧
      ((if everDue history grace then 1 else 0) +
        if due (observation :: history) grace && !everDue history grace then 1 else 0) =
        if everDue history grace || due (observation :: history) grace then 1 else 0
    cases prior : everDue history grace <;> cases deadline : due (observation :: history) grace <;>
      simp_all [checkpoint, intervalStart]
-- Read continuity directly from the trace: take the uninterrupted unavailable
-- suffix, then select its oldest observation (traces are newest first).
def suffixStart (history : List Observation) : Option Nat :=
  ((history.takeWhile unavailable).map Observation.time).getLast?

lemma suffix_start (history : List Observation) : intervalStart history = suffixStart history := by
  induction history with
  | nil => simp [intervalStart, suffixStart]
  | cons observation history induction =>
    cases available : unavailable observation <;>
      simp [intervalStart, suffixStart, available,
        List.getLast?_cons, induction]

-- Safety and liveness over every finite trace, including recovery and PR changes:
-- the current tick releases iff it is due and no earlier tick released. A live
-- current observation can never be due; a dead unprotected interval releases
-- on its first observed deadline. No assumption excludes failed/limited traces.
theorem correct (history : List Observation) (observation : Observation) (grace : Nat) :
    (run (observation :: history) grace).since = suffixStart (observation :: history) ∧
    (run (observation :: history) grace).released = everDue (observation :: history) grace ∧
    ((run (observation :: history) grace).commands = (run history grace).commands + 1 ↔
      due (observation :: history) grace = true ∧ everDue history grace = false) ∧
    (due (observation :: history) grace = true ↔
      unavailable observation = true ∧ claimProtected observation = false ∧
      ∃ start, suffixStart (observation :: history) = some start ∧
        grace ≤ observation.time - start) := by
  rw [← suffix_start]
  have prior := trace_invariant history grace
  have current := trace_invariant (observation :: history) grace
  refine ⟨current.1, current.2.1, ?_, ?_⟩
  · simp only [run, tick, prior.1, prior.2.1, checkpoint]
    change (_ + (if due (observation :: history) grace && !everDue history grace then 1 else 0) = _ + 1) ↔ _
    cases due (observation :: history) grace <;> cases everDue history grace <;> simp
  · cases start : intervalStart (observation :: history) <;> simp [due, releaseDue, start, and_assoc]

-- Unit cost per release command across a whole ownership epoch, not per tick.
-- Arbitrarily many observations cannot dispatch duplicate release commands.
theorem cost_bound (history : List Observation) (grace : Nat) :
    (run history grace).commands ≤ 1 := by
  rw [(trace_invariant history grace).2.2]
  split <;> omega

-- Alternatives are time-indexed release schedules that retain claims during
-- the required grace interval of a continuously failed, unprotected agent.
-- Cost is response latency from failure, in seconds. Any such schedule's first
-- (or any) release has latency at least ours; eventual schedules are included.
theorem optimal (history : List Observation) (observation : Observation) (start grace : Nat)
    (interval : intervalStart (observation :: history) = some start)
    (unavailableNow : unavailable observation = true)
    (unprotectedNow : claimProtected observation = false)
    (deadline : observation.time = start + grace)
    (held : everDue history grace = false)
    (alternative : Nat → Bool)
    (safe : ∀ elapsed, elapsed < grace → alternative elapsed = false)
    (elapsed : Nat) (releases : alternative elapsed = true) :
    (run (observation :: history) grace).commands = (run history grace).commands + 1 ∧
    observation.time - start ≤ elapsed := by
  constructor
  · apply (correct history observation grace).2.2.1.mpr
    exact ⟨by simp [due, releaseDue, interval, unavailableNow, unprotectedNow, deadline], held⟩
  · have lower : grace ≤ elapsed := by
      by_contra early
      have := safe elapsed (Nat.lt_of_not_ge early)
      simp_all
    omega

#print axioms correct
#print axioms cost_bound
#print axioms optimal

def actionName : Action → String
  | .nothing => "nothing" | .nudge => "nudge" | .restart => "restart"

def stallRows : String := String.join <| [299, 300, 301].flatMap fun elapsed =>
  [false, true].flatMap fun nudged => [false, true].map fun exempt =>
    s!"stall\t{elapsed}\t300\t{nudged}\t{exempt}\t{actionName (stall elapsed 300 nudged exempt)}\n"

def statusName : Status → String
  | .live => "live" | .failed => "failed" | .atLimit => "limit"
  | .parked => "parked" | .buildQueue => "queue" | .closed => "closed"

def traceRows (name : String) (observations : List Observation) : String := Id.run do
  let mut state : Checkpoint := {}
  let mut rows := ""
  for observation in observations do
    let next := tick state observation 600
    let since := next.since.map toString |>.getD "none"
    rows := rows ++ s!"trace\t{name}\t{observation.time}\t{statusName observation.status}\t{observation.pullRequest}\t{since}\t{decide (next.commands > state.commands)}\t{next.released}\n"
    state := next
  return rows

def traces : List (String × List Observation) := [
  ("failure", [{time := 100, status := .failed}, {time := 699, status := .failed},
    {time := 700, status := .failed}, {time := 701, status := .failed}]),
  ("recovery", [{time := 100, status := .failed}, {time := 699, status := .live},
    {time := 700, status := .failed}, {time := 1299, status := .failed},
    {time := 1300, status := .failed}]),
  ("limit", [{time := 100, status := .atLimit}, {time := 700, status := .atLimit}]),
  ("pr", [{time := 100, status := .failed, pullRequest := true},
    {time := 700, status := .failed, pullRequest := true}, {time := 701, status := .failed}]),
  ("exempt", [{time := 100, status := .failed}, {time := 700, status := .parked},
    {time := 800, status := .buildQueue}, {time := 900, status := .closed},
    {time := 1000, status := .live}, {time := 2000, status := .live}]),
  ("switch", [{time := 100, status := .failed}, {time := 699, status := .atLimit},
    {time := 700, status := .failed}])]

#eval IO.FS.writeFile "fixtures/agent-activity.tsv"
  (stallRows ++ String.join (traces.map fun (name, observations) => traceRows name observations))
end AgentActivity
