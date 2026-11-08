module
public import Std
public section

/-!
SPEC.md F17: Rust is extracted by Charon, translated by Aeneas, then checked by Lean.
This model covers the ordering of successful pipeline stages, not translator soundness
or PowerShell execution. Each stage invocation costs one unit; setup, file sizes and
the work inside a stage are outside this cost model.
-/

namespace Velocity.Toolchain

inductive Stage where
  | charon | aeneas | lean
  deriving DecidableEq, Repr

@[expose] def prerequisite : Stage → Nat
  | .charon => 0
  | .aeneas => 1
  | .lean => 2

@[expose] def advance (progress : Nat) (stage : Stage) : Nat :=
  if progress = prerequisite stage then progress + 1 else progress

@[expose] def run : List Stage → Nat → Nat
  | [], progress => progress
  | stage :: remaining, progress => run remaining (advance progress stage)

@[expose] def pipeline : List Stage := [.charon, .aeneas, .lean]

@[expose] def Spec (stages : List Stage) : Prop := run stages 0 = 3

theorem correct : Spec pipeline := by rfl

/-- Cost model: one unit per stage invocation, regardless of the stage's internal cost. -/
theorem cost_bound : pipeline.length ≤ 3 := by decide

private theorem advance_bound (progress : Nat) (stage : Stage) :
    advance progress stage ≤ progress + 1 := by
  simp only [advance]
  split <;> omega

private theorem run_bound (stages : List Stage) (progress : Nat) :
    run stages progress ≤ progress + stages.length := by
  induction stages generalizing progress with
  | nil => simp [run]
  | cons stage remaining inductionHypothesis =>
    have nextBound := inductionHypothesis (advance progress stage)
    have stepBound := advance_bound progress stage
    simp only [run, List.length_cons]
    omega

/-- Alternatives are all finite sequences of these three stages starting with Rust
only (progress zero) and reaching checked Lean (progress three). Under the cost
model of one unit per invocation, none uses fewer invocations than our pipeline. -/
theorem optimal (alternative : List Stage) (satisfiesSpec : Spec alternative) :
    pipeline.length ≤ alternative.length := by
  have bound := run_bound alternative 0
  simp only [Spec] at satisfiesSpec
  simp only [pipeline, List.length_cons, List.length_nil]
  omega

#print axioms correct
#print axioms cost_bound
#print axioms optimal

end Velocity.Toolchain
