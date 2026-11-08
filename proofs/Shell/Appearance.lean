module

public import Shell.Design

@[expose] public section

/-! SPEC.md F1 and F17: compiled reference model for the Close-hover regression.
The corrected PR-duty evidence failed on the Close/hover input. CSS has no
configured code translator, so the reference process supplies the proved model
for the reviewer's independent comparison with the actual stylesheet. This
module does not claim a formal model of a browser's cascade or renderer.
-/
namespace Velocity.Shell.Appearance

inductive Interaction where
  | rest | hover
  deriving DecidableEq

structure Style where
  background : String
  color : String
  width : Nat
  deriving DecidableEq

def referenceStyle (action : Action) (interaction : Interaction) : Style :=
  match interaction, action with
  | .hover, .close => ⟨"#c42b1c", "#fff", 44⟩
  | .hover, _ => ⟨"rgba(255,255,255,.1)", "#f5f5f7", 44⟩
  | .rest, _ => ⟨"none", "rgba(235,235,245,.62)", 44⟩

theorem close_hover_regression :
    referenceStyle .close .hover = ⟨"#c42b1c", "#fff", 44⟩ := by
  rfl

theorem control_width_regression (action : Action) (interaction : Interaction) :
    (referenceStyle action interaction).width = 44 := by
  cases interaction <;> cases action <;> rfl

theorem normal_close_is_not_danger_hover :
    referenceStyle .close .rest = ⟨"none", "rgba(235,235,245,.62)", 44⟩ := by
  rfl

#print axioms close_hover_regression
#print axioms control_width_regression
#print axioms normal_close_is_not_danger_hover

end Velocity.Shell.Appearance
