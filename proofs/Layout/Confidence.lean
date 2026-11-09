module
public import Layout.Design
public import Layout.Generated.Funs

@[expose] public section

/-!
SPEC.md F1 and §5. These theorems refer to Charon/Aeneas output from the actual
Rust layout and navigation policies, including their actual group/pulse order.
Numeric keys range over every machine usize; translating
them to Nat imposes no extra key-range assumption. Refines also requires normal
return, so a failing or divergent translation cannot satisfy the refinement.

Extract from PowerShell with `& ./proofs/scripts/Export-Rust.ps1 -CratePath
crates/velocity-core -Feature Layout -StartFrom @('crate::navigation',
'crate::layout', 'crate::layout_catalog::GROUPS', 'crate::layout_catalog::PULSE')`.

The search theorem checks every pair of normalized character slices, with no
length restriction beyond Rust's slice type. Its decreasing loop invariant proves
termination as well as matching. Unicode case normalization, string/JSON codecs,
read-model assembly and native/DOM effects are adapters outside this translation;
they do not strengthen the claims of these theorems.
-/

namespace Velocity.Layout.Confidence

open Aeneas Aeneas.Std Result

def decodeGroup : Generated.layout.ThreadGroup → Group
  | .Draft => .draft
  | .Duty => .duty
  | .Swarm => .swarm
  | .Spec => .spec
  | .Working => .working
  | .Review => .review
  | .Error => .error
  | .Closed => .closed
  | .Done => .done
  | .Merged => .merged

def decodeRow (row : Generated.layout.LayoutRow) : Row :=
  ⟨row.identifier.val, decodeGroup row.group, row.hidden, row.matches_search,
    row.member_of.map (·.val), row.expanded, row.agent_count.val, row.paused⟩

def decodePulse : Generated.layout.PulseKind → PulseKind
  | .Duty => .duty
  | .Spec => .spec
  | .Running => .running
  | .Watching => .watching
  | .Failed => .failed
  | .MergedToday => .mergedToday

theorem group_order_refines :
    Generated.layout_catalog.GROUPS.val.map decodeGroup = groupOrder := by
  simp [Generated.layout_catalog.GROUPS, Array.make, decodeGroup, groupOrder]

theorem pulse_order_refines :
    Generated.layout_catalog.PULSE.val.map (fun entry => decodePulse entry.1) = pulseOrder := by
  simp [Generated.layout_catalog.PULSE, Array.make, decodePulse, pulseOrder]

theorem group_index_refines (group : Generated.layout.ThreadGroup) :
    Generated.layout.group_index group ⦃ index => groupOrder[index.val]? = some (decodeGroup group) ⦄ := by
  cases group <;> simp [Generated.layout.group_index, groupOrder, decodeGroup]

theorem root_refines (group : Generated.layout.ThreadGroup) (row : Generated.layout.LayoutRow) :
    Generated.layout.is_root_visible group row = .ok (isRootVisible (decodeGroup group) (decodeRow row)) := by
  rcases row with ⟨identifier, rowGroup, hidden, searching, parent, expanded, agents, paused⟩
  cases group <;> cases rowGroup <;> cases hidden <;> cases searching <;>
    simp [Generated.layout.is_root_visible, Generated.layout.ThreadGroup.Insts.CoreCmpPartialEqThreadGroup.eq,
      isRootVisible, decodeRow, decodeGroup, Generated.layout.ThreadGroup.read_discriminant]

theorem member_refines (parent : Usize) (row : Generated.layout.LayoutRow) :
    Generated.layout.is_member_visible parent row =
      .ok ((decodeRow row).memberOf == some parent.val && !row.hidden) := by
  rcases row with ⟨identifier, group, hidden, searching, member, expanded, agents, paused⟩
  cases member with
  | none => rfl
  | some number =>
    by_cases same : number = parent
    · subst number; cases hidden <;> simp [Generated.layout.is_member_visible, decodeRow]
    · have different : number.val ≠ parent.val := by simpa only [UScalar.eq_equiv] using same
      simp [Generated.layout.is_member_visible, decodeRow, same, different]

theorem expansion_refines (row : Generated.layout.LayoutRow) :
    Generated.layout.expand_members row = .ok ((decodeRow row).group == .swarm && row.expanded) := by
  cases group : row.group <;> simp [Generated.layout.expand_members, decodeRow, group, decodeGroup]

theorem group_visibility_refines (group : Generated.layout.ThreadGroup) (hasRows searchEmpty : Bool) :
    Generated.layout.show_group group hasRows searchEmpty =
      .ok (hasRows || (decodeGroup group == .duty && searchEmpty)) := by
  cases group <;> cases hasRows <;> cases searchEmpty <;>
    simp [Generated.layout.show_group, decodeGroup]

theorem pulse_contribution_refines (kind : Generated.layout.PulseKind) (row : Generated.layout.LayoutRow) :
    Generated.layout.pulse_contribution kind row ⦃ value =>
      value.val = contribution (decodePulse kind) (decodeRow row) ⦄ := by
  rcases row with ⟨identifier, group, hidden, searching, member, expanded, agents, paused⟩
  cases kind <;> cases group <;> cases paused <;> simp [Generated.layout.pulse_contribution,
    decodeRow, decodeGroup, decodePulse, contribution, core.convert.num.FromUsizeBool.from]

theorem bounded_refines (count selected : Usize) :
    Generated.layout.bounded_selection count selected ⦃ value =>
      value.val = boundedSelection count.val selected.val ⦄ := by
  unfold Generated.layout.bounded_selection
  split
  · rename_i empty; simp_all [boundedSelection]
  · split
    · rename_i less; simp only [WP.spec_ok, boundedSelection]; scalar_tac
    · step; simp only [boundedSelection]; scalar_tac

private theorem next_model (count selected : Nat) :
    nextSelection count selected =
      if count = 0 then 0 else if boundedSelection count selected = count - 1 then 0
      else boundedSelection count selected + 1 := by
  unfold nextSelection
  split
  · rfl
  · rename_i nonempty
    have bounded := (bounded_selection_safe count selected).2 (by omega)
    split
    · rename_i last
      simp [last, show count - 1 + 1 = count by omega]
    · rw [Nat.mod_eq_of_lt (by omega)]

private theorem previous_model (count selected : Nat) :
    previousSelection count selected =
      if count = 0 then 0 else if boundedSelection count selected = 0 then count - 1
      else boundedSelection count selected - 1 := by
  unfold previousSelection
  split
  · rfl
  · rename_i nonempty
    have bounded := (bounded_selection_safe count selected).2 (by omega)
    split
    · rename_i first
      simp [first, Nat.mod_eq_of_lt (show count - 1 < count by omega)]
    · have sum : boundedSelection count selected + count - 1 =
          (boundedSelection count selected - 1) + count := by omega
      rw [sum, Nat.add_mod_right, Nat.mod_eq_of_lt (by omega)]

theorem next_refines (count selected : Usize) :
    Generated.layout.next_selection count selected ⦃ value =>
      value.val = nextSelection count.val selected.val ⦄ := by
  unfold Generated.layout.next_selection
  apply WP.spec_bind (bounded_refines count selected)
  intro bounded agrees
  have safe := bounded_selection_safe count.val selected.val
  rw [next_model]
  split
  · rename_i empty; simp_all
  · rename_i nonempty
    step as ⟨last, lastValue⟩
    split
    · rename_i atEnd; simp_all
    · rename_i beforeEnd
      step
      split_ifs <;> scalar_tac

theorem previous_refines (count selected : Usize) :
    Generated.layout.previous_selection count selected ⦃ value =>
      value.val = previousSelection count.val selected.val ⦄ := by
  unfold Generated.layout.previous_selection
  apply WP.spec_bind (bounded_refines count selected)
  intro bounded agrees
  have safe := bounded_selection_safe count.val selected.val
  rw [previous_model]
  split
  · rename_i empty; simp_all
  · split
    · step; split <;> scalar_tac
    · step
      simp only [← agrees]
      split_ifs <;> scalar_tac

def SearchInvariant (query label : Slice Char) (state : Usize × Usize) : Prop :=
  state.1.val ≤ query.length ∧ state.2.val ≤ label.length ∧
    fuzzy (query.val.drop state.1.val) (label.val.drop state.2.val) = fuzzy query.val label.val

theorem search_loop_refines (query label : Slice Char) (queryIndex labelIndex : Usize)
    (invariant : SearchInvariant query label (queryIndex, labelIndex)) :
    Generated.layout.fuzzy_match_loop query label queryIndex labelIndex ⦃ value =>
      value = fuzzy query.val label.val ⦄ := by
  have queryFits := Slice.length_ineq query
  have labelFits := Slice.length_ineq label
  unfold Generated.layout.fuzzy_match_loop
  apply loop.spec_decr_nat (fun state => label.length - state.2.val) (SearchInvariant query label)
  · rintro ⟨queryIndex, labelIndex⟩ ⟨queryBound, labelBound, agrees⟩
    dsimp only at queryBound labelBound agrees
    unfold Generated.layout.fuzzy_match_loop.body
    by_cases more : labelIndex.val < label.length
    · have more' : labelIndex < Slice.len label := by scalar_tac
      simp only [eq_true more', ↓reduceIte]
      by_cases complete : queryIndex.val = query.length
      · have complete' : queryIndex = Slice.len query := by scalar_tac
        simp only [eq_true complete', ↓reduceIte, WP.spec_ok]
        simpa [complete, fuzzy] using agrees
      · have incomplete : queryIndex ≠ Slice.len query := by scalar_tac
        have queryInside : queryIndex.val < query.length := by omega
        simp only [eq_false incomplete, ↓reduceIte]
        step as ⟨first, isFirst⟩
        step as ⟨character, isCharacter⟩
        rw [List.drop_eq_getElem_cons queryInside, List.drop_eq_getElem_cons more] at agrees
        simp only [← isFirst, ← isCharacter, fuzzy] at agrees
        split
        · rename_i equal
          step as ⟨nextQuery, nextQueryValue⟩
          step as ⟨nextLabel, nextLabelValue⟩
          simp only [SearchInvariant]
          exact ⟨⟨by omega, by omega, by simpa [equal, nextQueryValue, nextLabelValue] using agrees⟩, by omega⟩
        · rename_i different
          step as ⟨nextLabel, nextLabelValue⟩
          simp only [SearchInvariant]
          refine ⟨⟨queryBound, by omega, ?_⟩, by omega⟩
          rw [List.drop_eq_getElem_cons queryInside, ← isFirst]
          simpa [different, nextLabelValue] using agrees
    · have noMore : ¬ labelIndex < Slice.len label := by scalar_tac
      have endLabel : labelIndex.val = label.length := by omega
      simp only [eq_false noMore, ↓reduceIte, WP.spec_ok]
      rw [endLabel, List.drop_length] at agrees
      by_cases complete : queryIndex.val = query.length
      · have complete' : queryIndex = Slice.len query := by scalar_tac
        simpa [complete, complete', fuzzy] using agrees
      · have nonempty : queryIndex.val < query.length := by omega
        rw [List.drop_eq_getElem_cons nonempty, fuzzy] at agrees
        simpa [show queryIndex ≠ Slice.len query by scalar_tac] using agrees
  · exact invariant

theorem search_refines (query label : Slice Char) :
    Generated.layout.fuzzy_match query label ⦃ value => value = fuzzy query.val label.val ⦄ := by
  unfold Generated.layout.fuzzy_match
  apply search_loop_refines
  simp [SearchInvariant]

theorem alt_target_refines (number : Usize) (visible : Slice Usize) :
    Generated.layout.alt_target number visible ⦃ output =>
      output.map (·.val) = altTarget number.val (visible.val.map (·.val)) ⦄ := by
  unfold Generated.layout.alt_target
  by_cases lower : 1 ≤ number.val
  · have lower' : 1#usize ≤ number := by scalar_tac
    simp only [eq_true lower', ↓reduceIte]
    by_cases upper : number.val ≤ 9
    · have upper' : number ≤ 9#usize := by scalar_tac
      simp only [eq_true upper', ↓reduceIte]
      by_cases inside : number.val ≤ visible.length
      · have inside' : number ≤ Slice.len visible := by scalar_tac
        change number.val ≤ visible.val.length at inside
        simp only [eq_true inside', ↓reduceIte]
        step as ⟨index, isIndex⟩
        step as ⟨value, isValue⟩
        have inRange : index.val < visible.val.length := by omega
        simp [altTarget, lower, upper, List.getElem?_map,
          List.getElem?_eq_getElem inRange, ← isIndex, isValue]
      · have outside : ¬ number ≤ Slice.len visible := by scalar_tac
        change ¬ number.val ≤ visible.val.length at inside
        simp [outside, altTarget, lower, upper, List.getElem?_map,
          List.getElem?_eq_none (show visible.val.length ≤ number.val - 1 by omega)]
    · have above : ¬ number ≤ 9#usize := by scalar_tac
      simp [above, altTarget, upper]
  · have below : ¬ 1#usize ≤ number := by scalar_tac
    simp [below, altTarget, lower]

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
#print axioms group_index_refines
#print axioms group_order_refines
#print axioms pulse_order_refines
#print axioms root_refines
#print axioms member_refines
#print axioms expansion_refines
#print axioms group_visibility_refines
#print axioms pulse_contribution_refines
#print axioms bounded_refines
#print axioms next_refines
#print axioms previous_refines
#print axioms search_loop_refines
#print axioms search_refines
#print axioms alt_target_refines
#print axioms bare_key_refines
#print axioms decode_refines
#print axioms route_bare_refines
#print axioms route_refines
#print axioms shortcut_refines

end Velocity.Layout.Confidence
