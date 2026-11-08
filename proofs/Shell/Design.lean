module

import Std

public section

/-! SPEC.md F1 and §3: custom frameless window controls.

This model covers dispatch, not the operating system or the visual renderer.
Cost counts native window mutations; reading the current maximized state is
excluded. Alternatives are all finite native-operation traces containing the
operation required by a control request and the observed maximized state.
-/
namespace Velocity.Shell

inductive Action where
  | minimize | toggleMaximize | close
  deriving DecidableEq

inductive Operation where
  | minimize | maximize | restore | close
  deriving DecidableEq

def Spec (action : Action) (maximized : Bool) (operation : Operation) : Prop :=
  match action with
  | .minimize => operation = .minimize
  | .toggleMaximize => if maximized then operation = .restore else operation = .maximize
  | .close => operation = .close

def dispatch (action : Action) (maximized : Bool) : Operation :=
  match action with
  | .minimize => .minimize
  | .toggleMaximize => if maximized then .restore else .maximize
  | .close => .close

def mutationCost (trace : List Operation) : Nat := trace.length

def NativeDispatchTraces (action : Action) (maximized : Bool)
    (trace : List Operation) : Prop :=
  ∃ operation ∈ trace, Spec action maximized operation

theorem correct (action : Action) (maximized : Bool) :
    Spec action maximized (dispatch action maximized) := by
  cases action <;> cases maximized <;> rfl

theorem cost_bound (action : Action) (maximized : Bool) :
    mutationCost [dispatch action maximized] ≤ 1 := by
  simp [mutationCost]

theorem optimal (action : Action) (maximized : Bool) (alternative : List Operation)
    (valid : NativeDispatchTraces action maximized alternative) :
    mutationCost [dispatch action maximized] ≤ mutationCost alternative := by
  cases alternative with
  | nil => simp [NativeDispatchTraces] at valid
  | cons operation rest => simp [mutationCost]

#print axioms correct
#print axioms cost_bound
#print axioms optimal

end Velocity.Shell
