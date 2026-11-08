module

public import Shell.Design
public import Shell.Generated.Funs

@[expose] public section

/-! SPEC.md F1: code-level refinement of the actual Charon/Aeneas translation
of `velocity_core::window::window_operation`. Native window calls, webview
rendering and event transport remain external effects, outside this theorem.
-/
namespace Velocity.Shell.Confidence

def encodeAction : Action → Generated.window.WindowAction
  | .minimize => .Minimize
  | .toggleMaximize => .ToggleMaximize
  | .close => .Close

def encodeOperation : Operation → Generated.window.WindowOperation
  | .minimize => .Minimize
  | .maximize => .Maximize
  | .restore => .Restore
  | .close => .Close

def decodeAction : Generated.window.WindowAction → Action
  | .Minimize => .minimize
  | .ToggleMaximize => .toggleMaximize
  | .Close => .close

def decodeOperation : Generated.window.WindowOperation → Operation
  | .Minimize => .minimize
  | .Maximize => .maximize
  | .Restore => .restore
  | .Close => .close

theorem refines (action : Action) (maximized : Bool) :
    Generated.window.window_operation (encodeAction action) maximized =
      .ok (encodeOperation (dispatch action maximized)) := by
  cases action <;> cases maximized <;> rfl

theorem code_correct (action : Generated.window.WindowAction) (maximized : Bool) :
    ∃ operation, Generated.window.window_operation action maximized = .ok operation ∧
      Spec (decodeAction action) maximized (decodeOperation operation) := by
  cases action <;> cases maximized <;> exact ⟨_, rfl, rfl⟩

#print axioms refines
#print axioms code_correct

end Velocity.Shell.Confidence
