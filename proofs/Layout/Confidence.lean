module
public import Layout.Design
public import Layout.Generated.Funs

@[expose] public section

/-!
SPEC.md F1 and §5. These theorems refer to Charon/Aeneas output from the actual
Rust navigation module. Numeric keys range over every machine usize; translating
them to Nat imposes no extra key-range assumption. Refines also requires normal
return, so a failing or divergent translation cannot satisfy the refinement.

Extraction: `powershell -File proofs/scripts/Export-Rust.ps1 -CratePath
crates/velocity-core -Feature Layout -StartFrom crate::navigation`.
-/

namespace Velocity.Layout.Confidence

def decodeView : Generated.navigation.NavigationView → View
  | .Home => .home
  | .Thread => .thread
  | .SpecScan => .specScan
  | .SpecAsk => .specAsk
  | .SpecReady => .specReady
  | .SpecLocked => .specLocked
  | .Agents => .agents
  | .Settings => .settings
  | .Other => .other

def decodeIntent : Generated.navigation.NavigationIntent → Intent
  | .Palette => .palette
  | .NewThread => .newThread
  | .NewProject => .newProject
  | .Settings => .settings
  | .ShowNumbers => .showNumbers
  | .Jump number => .jump number.val
  | .Search => .search
  | .Task => .task
  | .Swarm => .swarm
  | .Goal => .goal
  | .Airc => .airc
  | .Dismiss => .dismiss
  | .Submit => .submit
  | .Dispatch => .dispatch
  | .Resume => .resume
  | .Fanout => .fanout
  | .PrDuty => .prDuty
  | .Scouts => .scouts
  | .Answer number => .answer number.val
  | .Lock => .lock
  | .SpecFile => .specFile
  | .Grill => .grill
  | .Diff => .diff
  | .Editor => .editor
  | .SaveAgents => .saveAgents

def decodeKey : Generated.navigation.ShortcutKey → Key
  | .K => .k
  | .N => .n
  | .Comma => .comma
  | .Alt => .alt
  | .Slash => .slash
  | .T => .t
  | .W => .w
  | .G => .g
  | .A => .a
  | .Escape => .escape
  | .Enter => .enter
  | .I => .i
  | .R => .r
  | .F => .f
  | .V => .v
  | .S => .s
  | .L => .l
  | .D => .d
  | .E => .e
  | .Digit number => .digit number.val
  | .Other => .other

def decodeStroke (stroke : Generated.navigation.KeyStroke) : KeyStroke :=
  ⟨decodeKey stroke.key, stroke.control, stroke.shift, stroke.alt, stroke.meta_held⟩

def decodeHint : Generated.navigation.NumberHintEvent → HintEvent
  | .AltDown => .altDown
  | .AltUp => .altUp
  | .Blur => .blur
  | .Other => .other

def Refines (program : Aeneas.Std.Result (Option Generated.navigation.NavigationIntent))
    (expected : Option Intent) : Prop :=
  ∃ output, program = .ok output ∧ output.map decodeIntent = expected

theorem hints_refine (shown : Bool) (event : Generated.navigation.NumberHintEvent) :
    Generated.navigation.update_number_hints shown event =
      .ok (numberHints shown (decodeHint event)) := by
  cases event <;> rfl

theorem bare_key_refines (view : Generated.navigation.NavigationView)
    (key : Generated.navigation.ShortcutKey) :
    Refines (Generated.navigation.bare_key_intent view key)
      (bareIntent (decodeView view) (decodeKey key)) := by
  cases view <;> cases key <;> exact ⟨_, rfl, rfl⟩

set_option maxHeartbeats 2000000 in
theorem decode_refines (view : Generated.navigation.NavigationView)
    (stroke : Generated.navigation.KeyStroke) :
    Refines (Generated.navigation.decode_key view stroke)
      (decode (decodeView view) (decodeStroke stroke)) := by
  rcases stroke with ⟨key, control, shift, alt, metaHeld⟩
  cases key <;> cases control <;> cases alt <;> cases metaHeld
  all_goals simp [Generated.navigation.decode_key, decode, decodeStroke, decodeKey]
  all_goals try exact bare_key_refines view .G
  all_goals try exact bare_key_refines view .S
  all_goals cases shift
  all_goals try simp only [Generated.navigation.control_key_intent]
  all_goals exact ⟨_, rfl, rfl⟩

theorem route_bare_refines (view : Generated.navigation.NavigationView)
    (intent : Generated.navigation.NavigationIntent) :
    Refines (Generated.navigation.route_bare_intent view intent)
      (routeBare (decodeView view) (decodeIntent intent)) := by
  cases view <;> cases intent <;>
    simp [Refines, Generated.navigation.route_bare_intent, Generated.navigation.select_if,
      routeBare, decodeView, decodeIntent]
  all_goals rename_i number
  all_goals by_cases lower : 1 ≤ (number : Nat) <;> by_cases upper : (number : Nat) ≤ 4 <;>
    simp [decodeIntent, lower, upper]

set_option maxHeartbeats 2000000 in
theorem route_refines (context : Generated.navigation.ShortcutContext) (controlChord : Bool)
    (intent : Generated.navigation.NavigationIntent) :
    Refines (Generated.navigation.route_intent context controlChord intent)
      (route (decodeView context.view) context.typing context.overlay_open context.form_open
        controlChord (decodeIntent intent)) := by
  rcases context with ⟨view, typing, overlay, form⟩
  cases intent <;> cases controlChord <;> cases typing <;> cases overlay <;> cases form
  all_goals simp [Generated.navigation.route_intent, Generated.navigation.select_if,
    route, decodeIntent]
  all_goals try exact route_bare_refines view _
  all_goals try exact ⟨_, rfl, rfl⟩
  all_goals first
    | (rename_i number
       by_cases lower : 1 ≤ (number : Nat) <;> by_cases upper : (number : Nat) ≤ 9 <;>
         simp [Refines, decodeIntent, lower, upper])
    | (cases view <;> simp [Refines, decodeView, decodeIntent])

theorem shortcut_refines (context : Generated.navigation.ShortcutContext)
    (stroke : Generated.navigation.KeyStroke) :
    Refines (Generated.navigation.shortcut_intent context stroke)
      (dispatch (decodeView context.view) context.typing context.overlay_open context.form_open
        (decodeStroke stroke)) := by
  obtain ⟨decoded, decodeProgram, decodeModel⟩ := decode_refines context.view stroke
  cases decoded with
  | none =>
    refine ⟨none, ?_, ?_⟩
    · simp [Generated.navigation.shortcut_intent, decodeProgram]
    · simp [dispatch, ← decodeModel]
  | some intent =>
    obtain ⟨output, routeProgram, routeModel⟩ :=
      route_refines context (stroke.control || stroke.alt) intent
    refine ⟨output, ?_, ?_⟩
    · cases control : stroke.control <;>
        simpa [Generated.navigation.shortcut_intent, decodeProgram, control] using routeProgram
    · change output.map decodeIntent =
        (decode (decodeView context.view) (decodeStroke stroke)).bind
          (route (decodeView context.view) context.typing context.overlay_open context.form_open
            (stroke.control || stroke.alt))
      rw [← decodeModel]
      exact routeModel

#print axioms hints_refine
#print axioms bare_key_refines
#print axioms decode_refines
#print axioms route_bare_refines
#print axioms route_refines
#print axioms shortcut_refines

end Velocity.Layout.Confidence
