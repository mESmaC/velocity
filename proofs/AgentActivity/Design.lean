module
public import Mathlib.Data.List.Basic
public section
-- SPEC.md section 2.2: stall escalation and claim release.
namespace AgentActivity
inductive Action where | nothing | nudge | restart | release deriving DecidableEq, Repr

def stall (silence timeout : Nat) (nudged exempt : Bool) : Action :=
  if exempt then .nothing else if silence < timeout then .nothing
  else if nudged then .restart else .nudge

def releaseDue (elapsed grace : Nat) (eligible protectedClaim : Bool) : Bool :=
  eligible && !protectedClaim && decide (grace ≤ elapsed)

theorem correct (silence timeout elapsed grace : Nat) (nudged exempt eligible protectedClaim : Bool) :
    (stall silence timeout nudged exempt = .nothing ↔ exempt = true ∨ silence < timeout) ∧
    (stall silence timeout nudged exempt = .nudge ↔ exempt = false ∧ timeout ≤ silence ∧ nudged = false) ∧
    (stall silence timeout nudged exempt = .restart ↔ exempt = false ∧ timeout ≤ silence ∧ nudged = true) ∧
    (releaseDue elapsed grace eligible protectedClaim = true ↔ eligible = true ∧ protectedClaim = false ∧ grace ≤ elapsed) := by
  cases nudged <;> cases exempt <;> cases eligible <;> cases protectedClaim <;>
    by_cases h : silence < timeout <;> by_cases g : grace ≤ elapsed <;>
    simp_all [stall, releaseDue, Nat.not_lt] <;> simp [Nat.not_lt.mpr h]

-- Cost counts dispatched recovery commands, not CPU time or tool execution.
def commands (action : Action) : List Action := if action = .nothing then [] else [action]
def cost (stalling releasing : List Action) : Nat := stalling.length + releasing.length

theorem cost_bound (silence timeout elapsed grace : Nat) (nudged exempt eligible protectedClaim : Bool) :
    cost (commands (stall silence timeout nudged exempt))
      (commands (if releaseDue elapsed grace eligible protectedClaim then .release else .nothing)) ≤ 2 := by
  unfold cost commands
  split <;> split <;> simp_all

lemma minimal_commands (action : Action) (alternative : List Action)
    (required : action ≠ .nothing → action ∈ alternative) :
    (commands action).length ≤ alternative.length := by
  unfold commands
  split
  · simp
  · simp only [List.length_singleton]
    have member := required (by assumption)
    have positive := List.length_pos_of_mem member
    omega

-- Alternatives must dispatch every required command in its stall/release lane.
-- With unit cost per dispatched command, extra commands cannot reduce cost.
theorem optimal (silence timeout elapsed grace : Nat) (nudged exempt eligible protectedClaim : Bool)
    (alternativeStall alternativeRelease : List Action)
    (stallRequired : stall silence timeout nudged exempt ≠ .nothing →
      stall silence timeout nudged exempt ∈ alternativeStall)
    (releaseRequired : (if releaseDue elapsed grace eligible protectedClaim then Action.release else .nothing) ≠ .nothing →
      (if releaseDue elapsed grace eligible protectedClaim then Action.release else .nothing) ∈ alternativeRelease) :
    cost (commands (stall silence timeout nudged exempt))
      (commands (if releaseDue elapsed grace eligible protectedClaim then .release else .nothing)) ≤
      cost alternativeStall alternativeRelease := by
  exact Nat.add_le_add (minimal_commands _ _ stallRequired) (minimal_commands _ _ releaseRequired)

#print axioms correct
#print axioms cost_bound
#print axioms optimal
def actionName : Action → String
  | .nothing => "nothing" | .nudge => "nudge" | .restart => "restart" | .release => "release"

def stallRows : String := String.join <| [299, 300, 301].flatMap fun elapsed =>
  [false, true].flatMap fun nudged => [false, true].map fun exempt =>
    s!"stall\t{elapsed}\t300\t{nudged}\t{exempt}\t{actionName (stall elapsed 300 nudged exempt)}\n"

def releaseRows : String := String.join <| [599, 600, 601].flatMap fun elapsed =>
  [false, true].flatMap fun eligible => [false, true].map fun protectedClaim =>
    s!"release\t{elapsed}\t600\t{eligible}\t{protectedClaim}\t{releaseDue elapsed 600 eligible protectedClaim}\n"

#eval IO.FS.writeFile "fixtures/agent-activity.tsv" (stallRows ++ releaseRows)
end AgentActivity

