module
public import Toolchain.Design
public import Mathlib.Data.List.Basic
public import Aeneas.Std.Primitives
public section

/-!
Bootstrap confidence at the model level (SPEC.md F17). The reference below composes
the design using the pinned Aeneas result type, checks its successful refinement,
and checks failure propagation. No Rust application exists yet to extract. These
theorems do not claim to verify PowerShell, the filesystem or external processes.
-/

namespace Velocity.Toolchain

open Aeneas.Std

@[expose] def checkedRun : List Stage → Nat → Result Nat
  | [], progress => .ok progress
  | stage :: remaining, progress =>
    Aeneas.Std.bind (.ok (advance progress stage)) (checkedRun remaining)

theorem checkedRun_refines (stages : List Stage) (progress : Nat) :
    checkedRun stages progress = .ok (run stages progress) := by
  induction stages generalizing progress with
  | nil => rfl
  | cons stage remaining inductionHypothesis =>
    simp only [checkedRun, Aeneas.Std.bind_ok, run]
    exact inductionHypothesis _

theorem checked_pipeline_correct : checkedRun pipeline 0 = .ok 3 := by
  rw [checkedRun_refines]
  rfl

theorem failure_stops_remaining (error : Error) (remaining : List Stage) :
    Aeneas.Std.bind (Result.fail error) (checkedRun remaining) = Result.fail error := by
  exact Aeneas.Std.bind_fail error (checkedRun remaining)

#print axioms checkedRun_refines
#print axioms checked_pipeline_correct
#print axioms failure_stops_remaining

end Velocity.Toolchain
