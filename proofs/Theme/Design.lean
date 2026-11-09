module
public section

/-
  Theme · design proof.

  Implements SPEC.md §6 (Visual language): "All tokens live on `:root` in the
  mockup. The Rust theme module mirrors them." and SPEC.md §3 (Architecture):
  "All visual values come from the design tokens in the mockup's `:root`, which
  the theme module reads."

  Design: `ui/src/lib/theme/tokens.css` is an exact copy of the mockup's `:root`
  block. The UI loads that file as is. The Rust theme module
  (`crates/velocity-core/src/theme.rs`) does not keep a second hand-written copy:
  it embeds the same file at compile time and reads its declarations with a
  one-pass, character-at-a-time scanner. The two sides therefore mirror each
  other by construction, provided the scanner returns exactly the declarations
  of the stylesheet it reads. That is `Spec` below.

  Cost model: the number of input characters a parser reads before it produces
  its result (`OnlineParser.cost`).

  Class of alternatives: every online parser (`OnlineParser`): any state type,
  any transition function, reading the input left to right one character at a
  time, and free to stop early with an answer at any character.
-/

namespace Velocity.Theme.Design

/-- One design token: `--name: value;` gives `⟨name, value⟩`, without the
leading `--`. -/
structure Token where
  name : List Char
  value : List Char
deriving DecidableEq, Repr

/-- One piece of a token stylesheet, such as `tokens.css`. -/
inductive Item where
  /-- `--name:` then `padding` spaces, then `value;`. -/
  | declaration (name : List Char) (padding : Nat) (value : List Char)
  /-- `/* text */`. -/
  | comment (text : List Char)
  /-- Any other single character: whitespace, `:root {`, `}`. -/
  | other (character : Char)

@[expose] def Item.render : Item → List Char
  | .declaration name padding value =>
      ['-', '-'] ++ name ++ [':'] ++ List.replicate padding ' ' ++ value ++ [';']
  | .comment text => ['/', '*'] ++ text ++ ['*', '/']
  | .other character => [character]

/-- The stylesheet text made of `items`. -/
@[expose] def render (items : List Item) : List Char := items.flatMap Item.render

@[expose] def Item.declarations : Item → List Token
  | .declaration name _ value => [⟨name, value⟩]
  | .comment _ => []
  | .other _ => []

/-- The tokens a stylesheet declares, in order. -/
@[expose] def declarations (items : List Item) : List Token := items.flatMap Item.declarations

/-- Whether `text` contains the comment terminator `*/`. -/
@[expose] def hasCommentClose : List Char → Bool
  | [] => false
  | [_] => false
  | first :: second :: rest =>
      (first == '*' && second == '/') || hasCommentClose (second :: rest)

/-- The shapes `tokens.css` is written in: names without `:`, values without `;`
that start with something other than a space, comments that don't close early,
and no stray `-` or `/` outside declarations and comments. -/
@[expose] def Item.WellFormed : Item → Prop
  | .declaration name _ value => ':' ∉ name ∧ ';' ∉ value ∧ value.head? ≠ some ' '
  | .comment text => hasCommentClose text = false
  | .other character => character ≠ '-' ∧ character ≠ '/'

@[expose] def WellFormed (items : List Item) : Prop := ∀ item ∈ items, item.WellFormed

/-- SPEC.md §6 and §3: the theme module holds exactly the tokens declared by the
stylesheet it reads, with the same names and values, in the same order. -/
@[expose] def Spec (parser : List Char → List Token) : Prop :=
  ∀ items, WellFormed items → parser (render items) = declarations items

/-! ## The design: a character-at-a-time scanner -/

inductive Mode where
  | outside
  /-- Read one `-` outside a declaration. -/
  | dash
  /-- Read one `/` outside a comment. -/
  | slash
  | comment
  /-- Inside a comment, just after a `*`. -/
  | commentStar
  | name (accumulated : List Char)
  /-- Between `:` and the first character of the value. -/
  | padding (name : List Char)
  | value (name : List Char) (accumulated : List Char)

structure State where
  mode : Mode
  tokens : List Token

@[expose] def outsideStep (character : Char) : Mode :=
  if character = '-' then .dash else if character = '/' then .slash else .outside

@[expose] def step (state : State) (character : Char) : State :=
  match state.mode with
  | .outside => { state with mode := outsideStep character }
  | .dash =>
      if character = '-' then { state with mode := .name [] }
      else { state with mode := outsideStep character }
  | .slash =>
      if character = '*' then { state with mode := .comment }
      else { state with mode := outsideStep character }
  | .comment =>
      if character = '*' then { state with mode := .commentStar }
      else { state with mode := .comment }
  | .commentStar =>
      if character = '/' then { state with mode := .outside }
      else if character = '*' then { state with mode := .commentStar }
      else { state with mode := .comment }
  | .name accumulated =>
      if character = ':' then { state with mode := .padding accumulated }
      else { state with mode := .name (accumulated ++ [character]) }
  | .padding name =>
      if character = ' ' then { state with mode := .padding name }
      else if character = ';' then
        { mode := .outside, tokens := state.tokens ++ [⟨name, []⟩] }
      else { state with mode := .value name [character] }
  | .value name accumulated =>
      if character = ';' then
        { mode := .outside, tokens := state.tokens ++ [⟨name, accumulated⟩] }
      else { state with mode := .value name (accumulated ++ [character]) }

@[expose] def start : State := ⟨.outside, []⟩

@[expose] def parse (input : List Char) : List Token := (input.foldl step start).tokens

/-! ## Correctness -/

theorem name_run (tokens : List Token) :
    ∀ (name accumulated : List Char), ':' ∉ name →
      name.foldl step ⟨.name accumulated, tokens⟩ = ⟨.name (accumulated ++ name), tokens⟩
  | [], accumulated, _ => by simp
  | character :: rest, accumulated, noColon => by
      have hHead : character ≠ ':' := fun h => noColon (h ▸ List.mem_cons_self)
      have hRest : ':' ∉ rest := fun h => noColon (List.mem_cons_of_mem _ h)
      simp only [List.foldl_cons, step, hHead, ite_false]
      rw [name_run tokens rest (accumulated ++ [character]) hRest]
      simp

theorem padding_run (name : List Char) (tokens : List Token) :
    ∀ count : Nat,
      (List.replicate count ' ').foldl step ⟨.padding name, tokens⟩ = ⟨.padding name, tokens⟩
  | 0 => rfl
  | count + 1 => by
      simp only [List.replicate_succ, List.foldl_cons, step, ite_true]
      exact padding_run name tokens count

theorem value_run (name : List Char) (tokens : List Token) :
    ∀ (value accumulated : List Char), ';' ∉ value →
      value.foldl step ⟨.value name accumulated, tokens⟩ =
        ⟨.value name (accumulated ++ value), tokens⟩
  | [], accumulated, _ => by simp
  | character :: rest, accumulated, noSemicolon => by
      have hHead : character ≠ ';' := fun h => noSemicolon (h ▸ List.mem_cons_self)
      have hRest : ';' ∉ rest := fun h => noSemicolon (List.mem_cons_of_mem _ h)
      simp only [List.foldl_cons, step, hHead, ite_false]
      rw [value_run name tokens rest (accumulated ++ [character]) hRest]
      simp

theorem declaration_run (tokens : List Token) (name : List Char) (padding : Nat)
    (value : List Char) (wellFormed : (Item.declaration name padding value).WellFormed) :
    (Item.declaration name padding value).render.foldl step ⟨.outside, tokens⟩ =
      ⟨.outside, tokens ++ [⟨name, value⟩]⟩ := by
  obtain ⟨noColon, noSemicolon, notSpace⟩ := wellFormed
  simp only [Item.render, List.foldl_append, List.foldl_cons, List.foldl_nil]
  simp only [step, outsideStep, ite_true]
  rw [name_run tokens name [] noColon]
  simp only [List.nil_append]
  rw [padding_run name tokens padding]
  cases value with
  | nil => simp
  | cons first rest =>
      have hSpace : first ≠ ' ' := fun h => notSpace (by simp [h])
      have hSemicolon : first ≠ ';' := fun h => noSemicolon (h ▸ List.mem_cons_self)
      have hRest : ';' ∉ rest := fun h => noSemicolon (List.mem_cons_of_mem _ h)
      simp only [List.foldl_cons, step, hSpace, hSemicolon, ite_false]
      rw [value_run name tokens rest [first] hRest]
      simp

theorem hasCommentClose_cons {character : Char} {rest : List Char}
    (noClose : hasCommentClose (character :: rest) = false) :
    hasCommentClose rest = false ∧ (character = '*' → rest.head? ≠ some '/') := by
  cases rest with
  | nil => simp [hasCommentClose]
  | cons second tail =>
      simp only [hasCommentClose, Bool.or_eq_false_iff, Bool.and_eq_false_iff,
        beq_eq_false_iff_ne, ne_eq] at noClose
      obtain ⟨noPair, noLater⟩ := noClose
      refine ⟨noLater, fun isStar => ?_⟩
      simp only [List.head?_cons, ne_eq, Option.some.injEq]
      intro isSlash
      cases noPair with
      | inl h => exact h isStar
      | inr h => exact h isSlash

theorem comment_run (tokens : List Token) :
    ∀ text : List Char, hasCommentClose text = false →
      (text ++ ['*', '/']).foldl step ⟨.comment, tokens⟩ = ⟨.outside, tokens⟩ ∧
      (text.head? ≠ some '/' →
        (text ++ ['*', '/']).foldl step ⟨.commentStar, tokens⟩ = ⟨.outside, tokens⟩)
  | [], _ => by simp [step]
  | character :: rest, noClose => by
      obtain ⟨restNoClose, starNotSlash⟩ := hasCommentClose_cons noClose
      obtain ⟨fromComment, fromStar⟩ := comment_run tokens rest restNoClose
      by_cases isStar : character = '*'
      · have afterStar := fromStar (starNotSlash isStar)
        subst isStar
        refine ⟨?_, fun _ => ?_⟩ <;>
          simpa [List.foldl_cons, step] using afterStar
      · refine ⟨?_, fun notSlash => ?_⟩
        · simpa [List.foldl_cons, step, isStar] using fromComment
        · have hSlash : character ≠ '/' := fun h => notSlash (by simp [h])
          simpa [List.foldl_cons, step, isStar, hSlash] using fromComment

theorem item_run (tokens : List Token) (item : Item) (wellFormed : item.WellFormed) :
    item.render.foldl step ⟨.outside, tokens⟩ = ⟨.outside, tokens ++ item.declarations⟩ := by
  cases item with
  | declaration name padding value =>
      simpa [Item.declarations] using declaration_run tokens name padding value wellFormed
  | comment text =>
      have closes := (comment_run tokens text wellFormed).1
      simp only [Item.render, List.foldl_append, List.foldl_cons, List.foldl_nil] at closes ⊢
      simpa [step, outsideStep, Item.declarations] using closes
  | other character =>
      obtain ⟨notDash, notSlash⟩ := wellFormed
      simp [Item.render, step, outsideStep, notDash, notSlash, Item.declarations]

theorem items_run :
    ∀ (items : List Item) (tokens : List Token), WellFormed items →
      (render items).foldl step ⟨.outside, tokens⟩ = ⟨.outside, tokens ++ declarations items⟩
  | [], tokens, _ => by simp [render, declarations]
  | item :: rest, tokens, wellFormed => by
      have headWellFormed := wellFormed item List.mem_cons_self
      have restWellFormed : WellFormed rest := fun other member =>
        wellFormed other (List.mem_cons_of_mem _ member)
      simp only [render, declarations, List.flatMap_cons, List.foldl_append] at ⊢
      rw [item_run tokens item headWellFormed]
      have restRun := items_run rest (tokens ++ item.declarations) restWellFormed
      simp only [render, declarations] at restRun
      rw [restRun, List.append_assoc]

/-- The scanner returns exactly the declarations of every well-formed stylesheet. -/
theorem correct : Spec parse := by
  intro items wellFormed
  have run := items_run items [] wellFormed
  simp only [parse, start, run, List.nil_append]

/-! ## Cost model and class of alternatives -/

/-- An online parser: it reads the input left to right, one character per
transition, and may stop early (`Sum.inr`) with its answer. -/
structure OnlineParser where
  State : Type
  start : State
  step : State → Char → State ⊕ List Token
  finish : State → List Token

/-- The answer and the number of characters read. -/
@[expose] def OnlineParser.runFrom (parser : OnlineParser) : parser.State → List Char → List Token × Nat
  | state, [] => (parser.finish state, 0)
  | state, character :: rest =>
      match parser.step state character with
      | .inr answer => (answer, 1)
      | .inl next =>
          let result := parser.runFrom next rest
          (result.1, result.2 + 1)

@[expose] def OnlineParser.output (parser : OnlineParser) (input : List Char) : List Token :=
  (parser.runFrom parser.start input).1

/-- Cost model: characters read before the parser answers. -/
@[expose] def OnlineParser.cost (parser : OnlineParser) (input : List Char) : Nat :=
  (parser.runFrom parser.start input).2

/-- The design, as an online parser. It never stops early. -/
@[expose] def design : OnlineParser where
  State := State
  start := start
  step := fun state character => .inl (step state character)
  finish := State.tokens

theorem design_runFrom :
    ∀ (input : List Char) (state : State),
      design.runFrom state input = ((input.foldl step state).tokens, input.length)
  | [], _ => rfl
  | character :: rest, state => by
      simp only [OnlineParser.runFrom, design, List.foldl_cons, List.length_cons]
      have restRun := design_runFrom rest (step state character)
      simp only [design] at restRun
      rw [restRun]

theorem design_output (input : List Char) : design.output input = parse input := by
  show (design.runFrom start input).1 = parse input
  rw [design_runFrom]
  rfl

theorem design_correct : Spec design.output := by
  intro items wellFormed
  rw [design_output]
  exact correct items wellFormed

/-- The design reads each character once. -/
theorem cost_bound : ∀ input, design.cost input ≤ input.length := by
  intro input
  show (design.runFrom start input).2 ≤ input.length
  rw [design_runFrom]
  exact Nat.le_refl _

/-- An online parser either reads its whole input, or its answer is already
fixed by what it has read, whatever follows. -/
theorem reads_all_or_fixed (parser : OnlineParser) :
    ∀ (input : List Char) (state : parser.State),
      (parser.runFrom state input).2 = input.length ∨
        ∀ suffix, parser.runFrom state (input ++ suffix) = parser.runFrom state input
  | [], _ => Or.inl rfl
  | character :: rest, state => by
      simp only [OnlineParser.runFrom, List.cons_append, List.length_cons]
      cases parser.step state character with
      | inr answer => exact Or.inr fun _ => rfl
      | inl next =>
          cases reads_all_or_fixed parser rest next with
          | inl readsAll => exact Or.inl (by simp [readsAll])
          | inr fixed => exact Or.inr fun suffix => by simp [fixed suffix]

/-- No online parser that meets `Spec` reads fewer characters than the design,
on any well-formed stylesheet: one that stopped early could not tell the
stylesheet apart from the same stylesheet with one more declaration. -/
theorem optimal :
    ∀ parser : OnlineParser, Spec parser.output →
      ∀ items, WellFormed items → design.cost (render items) ≤ parser.cost (render items) := by
  intro parser meetsSpec items wellFormed
  have designCost := cost_bound (render items)
  cases reads_all_or_fixed parser (render items) parser.start with
  | inl readsAll =>
      simp only [OnlineParser.cost, readsAll] at designCost ⊢
      exact designCost
  | inr fixed =>
      exfalso
      let extra : Item := .declaration ['x'] 0 ['y']
      have extraWellFormed : WellFormed (items ++ [extra]) := by
        intro item member
        rcases List.mem_append.mp member with inItems | isExtra
        · exact wellFormed item inItems
        · simp only [List.mem_singleton] at isExtra
          subst isExtra
          simp [extra, Item.WellFormed]
      have longer := meetsSpec (items ++ [extra]) extraWellFormed
      have shorter := meetsSpec items wellFormed
      have sameOutput : parser.output (render (items ++ [extra])) = parser.output (render items) := by
        simp only [OnlineParser.output, render, List.flatMap_append] at fixed ⊢
        rw [fixed]
      rw [sameOutput, shorter] at longer
      have lengths := congrArg List.length longer
      simp [declarations, extra, Item.declarations] at lengths

#print axioms correct
#print axioms design_output
#print axioms design_correct
#print axioms cost_bound
#print axioms reads_all_or_fixed
#print axioms optimal

end Velocity.Theme.Design
