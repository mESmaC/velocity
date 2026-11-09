module
public import Std
public section

/-!
SPEC.md F1, §2.1, §5 and §7 M1: main-window projection and keyboard navigation.
The model covers stable sidebar groups, expanded swarm members, zero-free pulse,
literal subsequence palette search, bounded selection and contextual shortcuts.
Rendering, persistence, provider processes and later feature actions are outside
this model; navigation returns an intent for their owning milestone to handle.

Search cost counts character-equality probes, excluding case normalization and
output allocation. The optimality class is every finite deterministic decision
tree that decides singleton-query membership using indexed equality probes only.
On an absent singleton query every such tree must inspect every label position.
-/

namespace Velocity.Layout

inductive Group where
  | draft | duty | swarm | spec | working | review | error | closed | done | merged
  deriving DecidableEq, Repr

@[expose] def groupOrder : List Group :=
  [.draft, .duty, .swarm, .spec, .working, .review, .error, .closed, .done, .merged]

structure Row where
  identifier : Nat
  group : Group
  hidden : Bool
  matchesSearch : Bool
  memberOf : Option Nat
  expanded : Bool
  agentCount : Nat
  paused : Bool
  deriving DecidableEq, Repr

@[expose] def isRootVisible (group : Group) (row : Row) : Bool :=
  row.group == group && !row.hidden && row.matchesSearch && row.memberOf.isNone

@[expose] def roots (group : Group) (rows : List Row) : List Row :=
  rows.filter (isRootVisible group)

@[expose] def members (parent : Row) (rows : List Row) : List Row :=
  rows.filter fun row => row.memberOf == some parent.identifier && !row.hidden

@[expose] def rowBlock (rows : List Row) (root : Row) : List Row :=
  root :: if root.group == .swarm && root.expanded then members root rows else []

@[expose] def sidebar (rows : List Row) : List Row :=
  groupOrder.flatMap fun group => (roots group rows).flatMap (rowBlock rows)

@[expose] def showGroup (group : Group) (rows : List Row) (searchEmpty : Bool) : Bool :=
  !(roots group rows).isEmpty || (group == .duty && searchEmpty)

theorem roots_exact (group : Group) (rows : List Row) (row : Row) :
    row ∈ roots group rows ↔ row ∈ rows ∧ row.group = group ∧ row.hidden = false ∧
      row.matchesSearch = true ∧ row.memberOf = none := by
  simp [roots, isRootVisible, Bool.and_eq_true, Option.isNone_iff_eq_none,
    and_assoc]

theorem roots_stable (group : Group) (rows : List Row) :
    (roots group rows).Sublist rows := List.filter_sublist

theorem members_exact (parent : Row) (rows : List Row) (row : Row) :
    row ∈ members parent rows ↔ row ∈ rows ∧
      row.memberOf = some parent.identifier ∧ row.hidden = false := by
  simp [members, Bool.and_eq_true]

theorem empty_duty_visible (rows : List Row) : showGroup .duty rows true = true := by
  simp [showGroup]

theorem collapsed_has_no_members (rows : List Row) (root : Row)
    (collapsed : root.expanded = false) : rowBlock rows root = [root] := by
  simp [rowBlock, collapsed]

theorem group_order_complete (group : Group) : group ∈ groupOrder := by
  cases group <;> simp [groupOrder]

theorem visible_root_appears (rows : List Row) (row : Row) (present : row ∈ rows)
    (visible : isRootVisible row.group row = true) : row ∈ sidebar rows := by
  apply List.mem_flatMap.mpr
  refine ⟨row.group, group_order_complete row.group, ?_⟩
  apply List.mem_flatMap.mpr
  exact ⟨row, List.mem_filter.mpr ⟨present, visible⟩, by simp [rowBlock]⟩

theorem empty_group_hidden (group : Group) (rows : List Row) (searchEmpty : Bool)
    (empty : roots group rows = []) (noDutyFallback : group ≠ .duty ∨ searchEmpty = false) :
    showGroup group rows searchEmpty = false := by
  rcases noDutyFallback with notDuty | searching <;> simp_all [showGroup]

inductive PulseKind where
  | duty | spec | running | watching | failed | mergedToday
  deriving DecidableEq, Repr

@[expose] def pulseOrder : List PulseKind :=
  [.duty, .spec, .running, .watching, .failed, .mergedToday]

@[expose] def contribution (kind : PulseKind) (row : Row) : Nat :=
  match kind with
  | .duty => if row.group == .duty && !row.paused then row.agentCount else 0
  | .spec => if row.group == .spec then 1 else 0
  | .running => if row.group == .working then 1 else 0
  | .watching => if row.group == .review then 1 else 0
  | .failed => if row.group == .error then 1 else 0
  | .mergedToday => 0

@[expose] def count (kind : PulseKind) (rows : List Row) (mergedToday : Nat) : Nat :=
  if kind == .mergedToday then mergedToday
  else (rows.map (contribution kind)).foldl (· + ·) 0

@[expose] def pulse (rows : List Row) (mergedToday : Nat) : List (PulseKind × Nat) :=
  (pulseOrder.map fun kind => (kind, count kind rows mergedToday)).filter
    fun entry => entry.2 != 0

theorem pulse_exact (rows : List Row) (mergedToday : Nat) (kind : PulseKind)
    (value : Nat) : (kind, value) ∈ pulse rows mergedToday ↔
      kind ∈ pulseOrder ∧ value = count kind rows mergedToday ∧ value ≠ 0 := by
  constructor
  · intro present
    obtain ⟨mapped, nonzero⟩ := List.mem_filter.mp present
    obtain ⟨candidate, ordered, equality⟩ := List.mem_map.mp mapped
    have sameKind := congrArg Prod.fst equality
    have sameCount := congrArg Prod.snd equality
    change candidate = kind at sameKind
    subst candidate
    exact ⟨ordered, sameCount.symm, by simpa using nonzero⟩
  · rintro ⟨ordered, rfl, nonzero⟩
    apply List.mem_filter.mpr
    exact ⟨List.mem_map.mpr ⟨kind, ordered, rfl⟩, by simpa using nonzero⟩

theorem pulse_hides_zero (rows : List Row) (mergedToday : Nat) (kind : PulseKind) :
    (kind, 0) ∉ pulse rows mergedToday := by
  simp [pulse_exact]

/- The caller normalizes case once; every character, including regex punctuation,
   is compared literally. No query is interpreted as a pattern or program. -/
@[expose] def fuzzy : List Char → List Char → Bool
  | [], _ => true
  | _ :: _, [] => false
  | first :: remaining, character :: rest =>
      if first = character then fuzzy remaining rest else fuzzy (first :: remaining) rest
termination_by _ label => label.length

@[expose] def searchCost : List Char → List Char → Nat
  | [], _ => 0
  | _ :: _, [] => 0
  | first :: remaining, character :: rest =>
      1 + if first = character then searchCost remaining rest
          else searchCost (first :: remaining) rest
termination_by _ label => label.length

theorem fuzzy_exact (query label : List Char) :
    fuzzy query label = true ↔ query.Sublist label := by
  induction label generalizing query with
  | nil => cases query <;> simp [fuzzy]
  | cons character rest inductionHypothesis =>
    cases query with
    | nil => simp [fuzzy]
    | cons first remaining =>
      by_cases equal : first = character
      · subst first
        simp [fuzzy, inductionHypothesis, List.cons_sublist_cons]
      · simp [fuzzy, equal, inductionHypothesis, List.sublist_cons_iff]

theorem cost_bound (query label : List Char) :
    searchCost query label ≤ label.length := by
  induction label generalizing query with
  | nil => cases query <;> simp [searchCost]
  | cons character rest inductionHypothesis =>
    cases query with
    | nil => simp [searchCost]
    | cons first remaining =>
      simp only [searchCost, List.length_cons]
      split <;> have bound := inductionHypothesis remaining <;>
        have sameBound := inductionHypothesis (first :: remaining) <;> omega

theorem absent_singleton_cost (query : Char) (label : List Char)
    (absent : query ∉ label) : searchCost [query] label = label.length := by
  induction label with
  | nil => simp [searchCost]
  | cons character rest inductionHypothesis =>
    simp only [List.mem_cons, not_or] at absent
    simp [searchCost, absent.1, inductionHypothesis absent.2, Nat.add_comm]

/- These trees may inspect in any order, repeat a probe or stop adaptively.
   They know the label length and learn its equality-to-query bits only by probes. -/
inductive SearchTree (length : Nat) where
  | answer (found : Bool)
  | inspect (index : Fin length) (equal different : SearchTree length)

@[expose] def evaluate {length : Nat} : SearchTree length → (Fin length → Bool) → Bool
  | .answer found, _ => found
  | .inspect index equal different, input =>
      if input index then evaluate equal input else evaluate different input

@[expose] def trace {length : Nat} : SearchTree length → (Fin length → Bool) → List (Fin length)
  | .answer _, _ => []
  | .inspect index equal different, input =>
      index :: if input index then trace equal input else trace different input

private theorem equal_on_trace {length : Nat} (tree : SearchTree length)
    (left right : Fin length → Bool)
    (agree : ∀ index ∈ trace tree left, left index = right index) :
    evaluate tree left = evaluate tree right := by
  induction tree with
  | answer found => rfl
  | inspect index equal different equalHypothesis differentHypothesis =>
    have atIndex := agree index (by simp [trace])
    simp only [evaluate, ← atIndex]
    cases value : left index
    · simp only [Bool.false_eq_true, ↓reduceIte]
      apply differentHypothesis
      intro position present
      exact agree position (by simp [trace, value, present])
    · simp only [↓reduceIte]
      apply equalHypothesis
      intro position present
      exact agree position (by simp [trace, value, present])

@[expose] def CorrectSearchTree {length : Nat} (tree : SearchTree length) : Prop :=
  ∀ input, evaluate tree input = true ↔ ∃ index, input index = true

private theorem correct_tree_reads_every_absent_position {length : Nat}
    (tree : SearchTree length) (correctTree : CorrectSearchTree tree)
    (index : Fin length) : index ∈ trace tree (fun _ => false) := by
  apply Classical.byContradiction
  intro missing
  let changed : Fin length → Bool := fun position => position == index
  have unchanged := equal_on_trace tree (fun _ => false) changed (by
    intro position present
    have different : position ≠ index := by
      intro equal
      exact missing (equal ▸ present)
    simp [changed, different])
  have absent : evaluate tree (fun _ => false) = false := by
    have specification := correctTree (fun _ => false)
    cases value : evaluate tree (fun _ => false) <;> simp_all
  have present : evaluate tree changed = true :=
    (correctTree changed).2 ⟨index, by simp [changed]⟩
  rw [absent, present] at unchanged
  contradiction

/-- Character-probe cost; alternatives are all correct finite indexed-equality
decision trees. For every absent singleton query our scan uses no more probes than
any alternative on its all-false input. This is a worst-case bound, not a claim
about ranking quality, normalization, rendering or multi-character lower bounds. -/
theorem optimal (query : Char) (label : List Char) (absent : query ∉ label)
    (alternative : SearchTree label.length) (correctAlternative : CorrectSearchTree alternative) :
    searchCost [query] label ≤ (trace alternative (fun _ => false)).length := by
  rw [absent_singleton_cost query label absent]
  have lowerBound := (List.nodup_finRange label.length).length_le_of_subset
    (fun index _ => correct_tree_reads_every_absent_position alternative correctAlternative index)
  simpa using lowerBound

@[expose] def paletteResults (query : List Char) (items : List (List Char)) : List (List Char) :=
  items.filter (fuzzy query)

theorem palette_exact (query : List Char) (items : List (List Char)) (label : List Char) :
    label ∈ paletteResults query items ↔ label ∈ items ∧ query.Sublist label := by
  simp [paletteResults, fuzzy_exact]

theorem palette_stable (query : List Char) (items : List (List Char)) :
    (paletteResults query items).Sublist items := List.filter_sublist

@[expose] def boundedSelection (count selected : Nat) : Nat :=
  min selected (count - 1)

@[expose] def nextSelection (count selected : Nat) : Nat :=
  if count = 0 then 0 else (boundedSelection count selected + 1) % count

@[expose] def previousSelection (count selected : Nat) : Nat :=
  if count = 0 then 0 else (boundedSelection count selected + count - 1) % count

theorem bounded_selection_safe (count selected : Nat) :
    (count = 0 → boundedSelection count selected = 0) ∧
    (0 < count → boundedSelection count selected < count) := by
  constructor
  · intro empty
    simp [boundedSelection, empty]
  · intro positive
    have bounded := Nat.min_le_right selected (count - 1)
    simp only [boundedSelection]
    omega

theorem palette_selection_safe (count selected : Nat) :
    (count = 0 → nextSelection count selected = 0 ∧ previousSelection count selected = 0) ∧
    (0 < count → nextSelection count selected < count ∧ previousSelection count selected < count) := by
  constructor
  · intro empty
    simp [nextSelection, previousSelection, empty]
  · intro positive
    simp only [nextSelection, previousSelection, Nat.ne_of_gt positive, ↓reduceIte]
    exact ⟨Nat.mod_lt _ positive, Nat.mod_lt _ positive⟩

@[expose] def altTarget {Item : Type} (number : Nat) (visible : List Item) : Option Item :=
  if 1 ≤ number ∧ number ≤ 9 then visible[number - 1]? else none

theorem alt_target_exact {Item : Type} (number : Nat) (visible : List Item) :
    (1 ≤ number ∧ number ≤ 9 → altTarget number visible = visible[number - 1]?) ∧
    (¬ (1 ≤ number ∧ number ≤ 9) → altTarget number visible = none) := by
  constructor
  · intro valid
    simp [altTarget, valid]
  · intro invalid
    simp [altTarget, invalid]

inductive View where
  | home | thread | specScan | specAsk | specReady | specLocked | agents | settings | other
  deriving DecidableEq, Repr

inductive Intent where
  | palette | newThread | newProject | settings | showNumbers | jump (number : Nat)
  | search | task | swarm | goal | airc | dismiss | submit
  | dispatch | resume | fanout | prDuty | scouts
  | answer (number : Nat) | lock | specFile | grill | diff | editor | saveAgents
  deriving DecidableEq, Repr

/- Chords are normalized by Rust before routing. A bare intent still has to pass
   its context guard. This prevents typing in any editable surface from launching
   work, and keeps S/G in an interview distinct from the home/global actions. -/
@[expose] def routeBare (view : View) (intent : Intent) : Option Intent :=
  match intent with
  | .search | .task | .swarm | .airc => some intent
  | .goal =>
      if view == .specScan || view == .specAsk || view == .specReady || view == .specLocked
      then none else some intent
  | .dispatch | .resume | .fanout | .prDuty | .scouts =>
      if view == .home then some intent else none
  | .answer number => if view == .specAsk && 1 ≤ number && number ≤ 4 then some intent else none
  | .lock | .specFile | .grill => if view == .specReady then some intent else none
  | .diff | .editor => if view == .thread then some intent else none
  | _ => none

@[expose] def route (view : View) (typing overlay form controlChord : Bool)
    (intent : Intent) : Option Intent :=
  match intent with
  | .dismiss => some .dismiss
  | .palette | .newThread | .newProject | .settings | .showNumbers =>
      if controlChord then some intent else none
  | .jump number =>
      if controlChord && 1 ≤ number && number ≤ 9 then some intent else none
  | .submit => if controlChord && form then some intent else none
  | .saveAgents => if controlChord && view == .agents then some intent else none
  | _ => if typing || overlay || controlChord then none else routeBare view intent

inductive Key where
  | k | n | comma | alt | slash | t | w | g | a | escape | enter
  | i | r | f | v | s | l | d | e | digit (number : Nat) | other
  deriving DecidableEq, Repr

structure KeyStroke where
  key : Key
  control : Bool
  shift : Bool
  alt : Bool
  metaHeld : Bool
  deriving DecidableEq, Repr

@[expose] def bareIntent (view : View) : Key → Option Intent
  | .slash => some .search
  | .t => some .task
  | .w => some .swarm
  | .g => some (if view == .specReady then .grill else .goal)
  | .a => some .airc
  | .i => some .dispatch
  | .r => some .resume
  | .f => some .fanout
  | .v => some .prDuty
  | .s => some (if view == .specReady then .specFile else .scouts)
  | .l => some .lock
  | .d => some .diff
  | .e => some .editor
  | .digit number => some (.answer number)
  | _ => none

@[expose] def decode (view : View) (stroke : KeyStroke) : Option Intent :=
  if stroke.key == .escape then some .dismiss
  else if stroke.metaHeld then none
  else if stroke.control then
    match stroke.key with
    | .k => some .palette
    | .n => some (if stroke.shift then .newProject else .newThread)
    | .comma => some .settings
    | .enter => some .submit
    | .s => some .saveAgents
    | _ => none
  else if stroke.alt then
    match stroke.key with
    | .alt => some .showNumbers
    | .digit number => some (.jump number)
    | _ => none
  else bareIntent view stroke.key

@[expose] def dispatch (view : View) (typing overlay form : Bool)
    (stroke : KeyStroke) : Option Intent :=
  (decode view stroke).bind (route view typing overlay form (stroke.control || stroke.alt))

theorem shortcut_new_project (view : View) (typing overlay form : Bool) :
    dispatch view typing overlay form ⟨.n, true, true, false, false⟩ = some .newProject := by
  rfl

theorem shortcut_interview_keys (typing overlay form : Bool) :
    dispatch .specReady typing overlay form ⟨.g, false, false, false, false⟩ =
      (if typing || overlay then none else some .grill) ∧
    dispatch .specReady typing overlay form ⟨.s, false, false, false, false⟩ =
      (if typing || overlay then none else some .specFile) := by
  simp [dispatch, decode, bareIntent, route, routeBare]

theorem shortcut_typing_guard (view : View) (key : Key) (shift overlay form : Bool) :
    dispatch view true overlay form ⟨key, false, shift, false, false⟩ =
      (if key == .escape then some .dismiss else none) := by
  cases key <;> cases view <;> simp [dispatch, decode, bareIntent, route]

inductive HintEvent where
  | altDown | altUp | blur | other
  deriving DecidableEq, Repr

@[expose] def numberHints (shown : Bool) : HintEvent → Bool
  | .altDown => true
  | .altUp | .blur => false
  | .other => shown

theorem number_hints_reset (shown : Bool) :
    numberHints shown .altUp = false ∧ numberHints shown .blur = false := by
  exact ⟨rfl, rfl⟩

@[expose] def Spec : Prop :=
  groupOrder = [.draft, .duty, .swarm, .spec, .working, .review, .error, .closed, .done, .merged] ∧
  (∀ group rows row, row ∈ roots group rows ↔ row ∈ rows ∧ row.group = group ∧
    row.hidden = false ∧ row.matchesSearch = true ∧ row.memberOf = none) ∧
  (∀ query label, fuzzy query label = true ↔ query.Sublist label) ∧
  (∀ rows today kind, (kind, 0) ∉ pulse rows today) ∧
  (∀ view overlay form chord, route view true overlay form chord .search = none ∧
    route view true overlay form chord .task = none ∧
    route view true overlay form chord .swarm = none ∧
    route view true overlay form chord .airc = none) ∧
  (∀ view typing overlay form, route view typing overlay form true .palette = some .palette) ∧
  (∀ typing overlay form chord, route .specReady typing overlay form chord .goal = none) ∧
  (∀ typing overlay form chord, route .specAsk typing overlay form chord .goal = none) ∧
  (∀ typing overlay form chord, route .specScan typing overlay form chord .goal = none) ∧
  (∀ typing overlay form chord, route .specLocked typing overlay form chord .goal = none)

theorem correct : Spec := by
  refine ⟨rfl, roots_exact, fuzzy_exact, pulse_hides_zero, ?_, ?_, ?_, ?_, ?_, ?_⟩
  · intros
    simp [route]
  · intros
    rfl
  · intros
    simp [route, routeBare]
  · intros
    simp [route, routeBare]
  · intros
    simp [route, routeBare]
  · intros
    simp [route, routeBare]

theorem dismiss_anywhere (view : View) (typing overlay form chord : Bool) :
    route view typing overlay form chord .dismiss = some .dismiss := rfl

theorem spec_actions_do_not_leak (view : View) (notReady : view ≠ .specReady) :
    routeBare view .lock = none ∧ routeBare view .specFile = none ∧
      routeBare view .grill = none := by
  simp [routeBare, notReady]

#print axioms roots_exact
#print axioms roots_stable
#print axioms members_exact
#print axioms empty_duty_visible
#print axioms collapsed_has_no_members
#print axioms group_order_complete
#print axioms visible_root_appears
#print axioms empty_group_hidden
#print axioms pulse_exact
#print axioms pulse_hides_zero
#print axioms fuzzy_exact
#print axioms cost_bound
#print axioms absent_singleton_cost
#print axioms optimal
#print axioms palette_exact
#print axioms palette_stable
#print axioms bounded_selection_safe
#print axioms palette_selection_safe
#print axioms alt_target_exact
#print axioms correct
#print axioms dismiss_anywhere
#print axioms spec_actions_do_not_leak
#print axioms shortcut_new_project
#print axioms shortcut_interview_keys
#print axioms shortcut_typing_guard
#print axioms number_hints_reset

end Velocity.Layout
