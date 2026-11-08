module
public import Theme.Design
public import Theme.Generated.Funs
public section

/-!
Theme · confidence proof (SPEC.md §6, §3; F17 level: code, Rust through Aeneas).

`Theme/Generated` is Aeneas' translation of `crates/velocity-core/src/theme.rs`.
This file proves that the translated scanner, `theme.scan_token_spans`, refines
the design scanner `Design.parse`: on every byte slice it returns (it never
panics or overflows) one span per token `Design.parse` finds, with the same name
and value, in the same order. Its loop reads each byte once, as `cost_bound`
counts.

Bytes and characters: the Rust code scans UTF-8 bytes; the design scans
characters. `character` reads one byte as the character with that code. For
ASCII text, such as every declaration in `tokens.css`, that is the text itself.
Bytes of multi-byte characters (≥ 0x80) become characters that the scanner never
reacts to, so they can only sit inside names, values or comments.

Not translated: `TOKENS_CSS` (the embedded file; its literal is too large for
Lean to elaborate) and `tokens()`, which only passes `TOKENS_CSS` to
`scan_token_spans`. `Cargo.toml` excludes both from Charon
(`[package.metadata.charon]`).
-/

namespace Velocity.Theme.Confidence

open Aeneas Aeneas.Std Result
open Velocity.Theme.Generated
open Velocity.Theme.Design (Token Mode State step start parse)

/-! ## Bytes as characters -/

/-- One byte read as the character with that code. -/
@[expose] def character (byte : U8) : Char := Char.ofNat byte.val

@[expose] def characters (css : Slice U8) : List Char := css.val.map character

/-- The characters of `css[first..last]`. -/
@[expose] def segment (css : Slice U8) (first last : Nat) : List Char :=
  ((css.val.drop first).take (last - first)).map character

theorem character_toNat (byte : U8) : (character byte).toNat = byte.val := by
  have bound : byte.val < 256 := by scalar_tac
  unfold character
  simp [Char.ofNat, Nat.isValidChar, show byte.val < 55296 by omega, Char.ofNatAux,
    Char.toNat]

theorem character_inj (first second : U8) : character first = character second ↔ first = second := by
  constructor
  · intro same
    have values := congrArg Char.toNat same
    rw [character_toNat, character_toNat] at values
    exact (UScalar.eq_equiv first second).mpr values
  · intro same
    rw [same]

theorem is_dash (byte : U8) : character byte = '-' ↔ byte = 45#u8 := by
  rw [show '-' = character 45#u8 by decide, character_inj]

theorem is_slash (byte : U8) : character byte = '/' ↔ byte = 47#u8 := by
  rw [show '/' = character 47#u8 by decide, character_inj]

theorem is_star (byte : U8) : character byte = '*' ↔ byte = 42#u8 := by
  rw [show '*' = character 42#u8 by decide, character_inj]

theorem is_colon (byte : U8) : character byte = ':' ↔ byte = 58#u8 := by
  rw [show ':' = character 58#u8 by decide, character_inj]

theorem is_space (byte : U8) : character byte = ' ' ↔ byte = 32#u8 := by
  rw [show ' ' = character 32#u8 by decide, character_inj]

theorem is_semicolon (byte : U8) : character byte = ';' ↔ byte = 59#u8 := by
  rw [show ';' = character 59#u8 by decide, character_inj]

theorem segment_self (css : Slice U8) (index : Nat) : segment css index index = [] := by
  simp [segment]

theorem segment_succ (css : Slice U8) (first index : Nat) (ordered : first ≤ index)
    (inside : index < css.length) :
    segment css first (index + 1) = segment css first index ++ [character css.val[index]] := by
  unfold segment
  rw [show index + 1 - first = (index - first) + 1 by omega, List.take_add_one,
    List.getElem?_drop, show first + (index - first) = index by omega,
    List.getElem?_eq_getElem inside]
  simp

/-! ## Abstraction from the Rust state to the design state -/

/-- The design mode a Rust mode stands for, once `index` bytes have been read. -/
@[expose] def abstractMode (css : Slice U8) (index : Nat) : theme.ScanMode → Mode
  | .Outside => .outside
  | .Dash => .dash
  | .Slash => .slash
  | .Comment => .comment
  | .CommentStar => .commentStar
  | .Name nameStart => .name (segment css nameStart.val index)
  | .Padding nameStart nameEnd => .padding (segment css nameStart.val nameEnd.val)
  | .Value nameStart nameEnd valueStart =>
      .value (segment css nameStart.val nameEnd.val) (segment css valueStart.val index)

/-- The token a Rust span stands for. -/
@[expose] def spanToken (css : Slice U8) (span : theme.TokenSpan) : Token :=
  ⟨segment css span.name_start.val span.name_end.val,
    segment css span.value_start.val span.value_end.val⟩

@[expose] def abstractState (css : Slice U8) (index : Nat) (mode : theme.ScanMode)
    (spans : alloc.vec.Vec theme.TokenSpan) : State :=
  ⟨abstractMode css index mode, spans.val.map (spanToken css)⟩

/-- Offsets of the segment still growing never run past the bytes read. -/
@[expose] def Bounded (index : Nat) : theme.ScanMode → Prop
  | .Name nameStart => nameStart.val ≤ index
  | .Value _ _ valueStart => valueStart.val ≤ index
  | _ => True

/-- What one translated step must do on `byte`: match one design step on the
same character, keep the offsets bounded, and push at most one span. -/
@[expose] def StepPost (css : Slice U8) (index : Nat) (byte : U8) (mode : theme.ScanMode)
    (spans : alloc.vec.Vec theme.TokenSpan)
    (result : theme.ScanMode × alloc.vec.Vec theme.TokenSpan) : Prop :=
  abstractState css (index + 1) result.1 result.2 =
      step (abstractState css index mode spans) (character byte) ∧
    Bounded (index + 1) result.1 ∧ result.2.length ≤ spans.length + 1

theorem outside_step_eq (byte : U8) :
    theme.outside_step byte =
      ok (if byte = 45#u8 then .Dash else if byte = 47#u8 then .Slash else .Outside) := by
  unfold theme.outside_step
  split_ifs <;> rfl

theorem abstract_outside_step (css : Slice U8) (index : Nat) (byte : U8) :
    abstractMode css index
        (if byte = 45#u8 then .Dash else if byte = 47#u8 then .Slash else .Outside) =
      Design.outsideStep (character byte) := by
  unfold Design.outsideStep
  by_cases isDash : byte = 45#u8
  · simp [isDash, abstractMode, is_dash]
  · by_cases isSlash : byte = 47#u8
    · simp [isSlash, abstractMode, is_dash, is_slash]
    · simp [isDash, isSlash, abstractMode, is_dash, is_slash]

theorem bounded_outside_step (index : Nat) (byte : U8) :
    Bounded index (if byte = 45#u8 then .Dash else if byte = 47#u8 then .Slash else .Outside) := by
  split_ifs <;> trivial

theorem scan_step_outside (css : Slice U8) (index : Usize) (byte : U8)
    (spans : alloc.vec.Vec theme.TokenSpan) :
    theme.scan_step .Outside byte index spans ⦃ result =>
      StepPost css index.val byte .Outside spans result ⦄ := by
  simp only [theme.scan_step, outside_step_eq, bind_ok, WP.spec_ok, StepPost, abstractState]
  refine ⟨?_, bounded_outside_step _ _, by omega⟩
  rw [abstract_outside_step]
  rfl

theorem scan_step_dash (css : Slice U8) (index : Usize) (byte : U8)
    (spans : alloc.vec.Vec theme.TokenSpan) (inside : index.val < css.length) :
    theme.scan_step .Dash byte index spans ⦃ result =>
      StepPost css index.val byte .Dash spans result ⦄ := by
  have fits := Slice.length_ineq css
  unfold theme.scan_step
  by_cases isDash : byte = 45#u8
  · simp only [eq_true isDash, ↓reduceIte]
    step as ⟨next, hNext⟩
    simp only [StepPost, abstractState]
    refine ⟨?_, ?_, by omega⟩
    · simp [step, abstractMode, hNext, segment_self, is_dash, isDash]
    · simp [Bounded, hNext]
  · simp only [eq_false isDash, ↓reduceIte]
    simp only [outside_step_eq, bind_ok, WP.spec_ok, StepPost, abstractState]
    refine ⟨?_, bounded_outside_step _ _, by omega⟩
    rw [abstract_outside_step]
    simp [step, abstractMode, is_dash, isDash]

theorem scan_step_slash (css : Slice U8) (index : Usize) (byte : U8)
    (spans : alloc.vec.Vec theme.TokenSpan) :
    theme.scan_step .Slash byte index spans ⦃ result =>
      StepPost css index.val byte .Slash spans result ⦄ := by
  simp only [theme.scan_step, theme.comment_step]
  by_cases isStar : byte = 42#u8
  · simp [isStar, StepPost, abstractState, abstractMode, step, is_star, Bounded]
  · simp only [eq_false isStar, ↓reduceIte]
    simp only [outside_step_eq, bind_ok, WP.spec_ok, StepPost, abstractState]
    refine ⟨?_, bounded_outside_step _ _, by omega⟩
    rw [abstract_outside_step]
    simp [step, abstractMode, is_star, isStar]

theorem scan_step_comment (css : Slice U8) (index : Usize) (byte : U8)
    (spans : alloc.vec.Vec theme.TokenSpan) :
    theme.scan_step .Comment byte index spans ⦃ result =>
      StepPost css index.val byte .Comment spans result ⦄ := by
  simp only [theme.scan_step, theme.comment_step]
  by_cases isStar : byte = 42#u8 <;>
    simp [isStar, StepPost, abstractState, abstractMode, step, is_star, Bounded]

theorem scan_step_commentStar (css : Slice U8) (index : Usize) (byte : U8)
    (spans : alloc.vec.Vec theme.TokenSpan) :
    theme.scan_step .CommentStar byte index spans ⦃ result =>
      StepPost css index.val byte .CommentStar spans result ⦄ := by
  simp only [theme.scan_step, theme.comment_step]
  by_cases isSlash : byte = 47#u8
  · simp [isSlash, StepPost, abstractState, abstractMode, step, is_slash, Bounded]
  · by_cases isStar : byte = 42#u8 <;>
      simp [isSlash, isStar, StepPost, abstractState, abstractMode, step, is_slash, is_star,
        Bounded]

theorem scan_step_name (css : Slice U8) (index : Usize) (byte : U8)
    (spans : alloc.vec.Vec theme.TokenSpan) (nameStart : Usize)
    (inside : index.val < css.length) (isByte : css.val[index.val] = byte)
    (bounded : nameStart.val ≤ index.val) :
    theme.scan_step (.Name nameStart) byte index spans ⦃ result =>
      StepPost css index.val byte (.Name nameStart) spans result ⦄ := by
  simp only [theme.scan_step, theme.declaration_step]
  by_cases isColon : byte = 58#u8
  · simp [isColon, StepPost, abstractState, abstractMode, step, is_colon, Bounded]
  · have grown := segment_succ css nameStart.val index.val bounded inside
    rw [isByte] at grown
    simp [isColon, StepPost, abstractState, abstractMode, step, is_colon, Bounded, grown]
    omega

theorem scan_step_padding (css : Slice U8) (index : Usize) (byte : U8)
    (spans : alloc.vec.Vec theme.TokenSpan) (nameStart nameEnd : Usize)
    (inside : index.val < css.length) (isByte : css.val[index.val] = byte)
    (room : spans.length ≤ index.val) :
    theme.scan_step (.Padding nameStart nameEnd) byte index spans ⦃ result =>
      StepPost css index.val byte (.Padding nameStart nameEnd) spans result ⦄ := by
  have fits := Slice.length_ineq css
  simp only [theme.scan_step, theme.declaration_step]
  by_cases isSpace : byte = 32#u8
  · simp [isSpace, StepPost, abstractState, abstractMode, step, is_space, Bounded]
  · by_cases isSemicolon : byte = 59#u8
    · simp only [eq_false isSpace, eq_true isSemicolon, ↓reduceIte]
      step as ⟨pushed, hPushed⟩
      simp [StepPost, abstractState, abstractMode, step, is_space, is_semicolon,
        isSemicolon, Bounded, hPushed, spanToken, segment_self]
    · have grown := segment_succ css index.val index.val (Nat.le_refl _) inside
      rw [isByte, segment_self] at grown
      simp [isSpace, isSemicolon, StepPost, abstractState, abstractMode, step, is_space,
        is_semicolon, Bounded, grown]

theorem scan_step_value (css : Slice U8) (index : Usize) (byte : U8)
    (spans : alloc.vec.Vec theme.TokenSpan) (nameStart nameEnd valueStart : Usize)
    (inside : index.val < css.length) (isByte : css.val[index.val] = byte)
    (bounded : valueStart.val ≤ index.val) (room : spans.length ≤ index.val) :
    theme.scan_step (.Value nameStart nameEnd valueStart) byte index spans ⦃ result =>
      StepPost css index.val byte (.Value nameStart nameEnd valueStart) spans result ⦄ := by
  have fits := Slice.length_ineq css
  simp only [theme.scan_step, theme.declaration_step]
  by_cases isSemicolon : byte = 59#u8
  · simp only [eq_true isSemicolon, ↓reduceIte]
    step as ⟨pushed, hPushed⟩
    simp [StepPost, abstractState, abstractMode, step, is_semicolon, isSemicolon, Bounded,
      hPushed, spanToken]
  · have grown := segment_succ css valueStart.val index.val bounded inside
    rw [isByte] at grown
    simp [isSemicolon, StepPost, abstractState, abstractMode, step, is_semicolon, Bounded, grown]
    omega

theorem scan_step_refines (css : Slice U8) (mode : theme.ScanMode) (index : Usize) (byte : U8)
    (spans : alloc.vec.Vec theme.TokenSpan) (inside : index.val < css.length)
    (isByte : css.val[index.val] = byte) (bounded : Bounded index.val mode)
    (room : spans.length ≤ index.val) :
    theme.scan_step mode byte index spans ⦃ result =>
      StepPost css index.val byte mode spans result ⦄ := by
  cases mode with
  | Outside => exact scan_step_outside css index byte spans
  | Dash => exact scan_step_dash css index byte spans inside
  | Slash => exact scan_step_slash css index byte spans
  | Comment => exact scan_step_comment css index byte spans
  | CommentStar => exact scan_step_commentStar css index byte spans
  | Name nameStart => exact scan_step_name css index byte spans nameStart inside isByte bounded
  | Padding nameStart nameEnd =>
      exact scan_step_padding css index byte spans nameStart nameEnd inside isByte room
  | Value nameStart nameEnd valueStart =>
      exact scan_step_value css index byte spans nameStart nameEnd valueStart inside isByte
        bounded room

/-! ## The loop -/

/-- After `index` bytes, the Rust state stands for the design state reached on
the same bytes. -/
@[expose] def LoopInvariant (css : Slice U8)
    (state : alloc.vec.Vec theme.TokenSpan × theme.ScanMode × Usize) : Prop :=
  state.2.2.val ≤ css.length ∧ Bounded state.2.2.val state.2.1 ∧
    state.1.length ≤ state.2.2.val ∧
    abstractState css state.2.2.val state.2.1 state.1 =
      ((characters css).take state.2.2.val).foldl step start

theorem characters_take_succ (css : Slice U8) (index : Nat) (inside : index < css.length) :
    (characters css).take (index + 1) =
      (characters css).take index ++ [character css.val[index]] := by
  simp [characters, List.take_add_one, List.getElem?_eq_getElem inside]

theorem scan_token_spans_loop_refines (css : Slice U8)
    (spans : alloc.vec.Vec theme.TokenSpan) (mode : theme.ScanMode) (index : Usize)
    (invariant : LoopInvariant css (spans, mode, index)) :
    theme.scan_token_spans_loop css spans mode index ⦃ result =>
      result.val.map (spanToken css) = parse (characters css) ⦄ := by
  have fits := Slice.length_ineq css
  unfold theme.scan_token_spans_loop
  apply loop.spec_decr_nat (fun state => css.length - state.2.2.val) (LoopInvariant css)
  · rintro ⟨spans, mode, index⟩ ⟨read, bounded, room, abstracts⟩
    dsimp only at read bounded room abstracts
    unfold theme.scan_token_spans_loop.body
    dsimp only
    by_cases more : index.val < css.length
    · have more' : index < Slice.len css := by scalar_tac
      simp only [eq_true more', ↓reduceIte]
      step as ⟨byte, isByte⟩
      have stepped := scan_step_refines css mode index byte spans more isByte.symm bounded room
      apply WP.spec_bind stepped
      rintro ⟨next, nextSpans⟩ ⟨agrees, nextBounded, grew⟩
      dsimp only at agrees nextBounded grew ⊢
      step as ⟨nextIndex, isNext⟩
      simp only [LoopInvariant]
      refine ⟨⟨by omega, by rw [isNext]; exact nextBounded, by omega, ?_⟩, by omega⟩
      rw [isNext, agrees, abstracts, characters_take_succ css index.val more, List.foldl_append,
        ← isByte]
      rfl
    · have done' : ¬ index < Slice.len css := by scalar_tac
      simp only [eq_false done', ↓reduceIte, WP.spec_ok]
      have whole : index.val = css.length := by omega
      have tokens := congrArg State.tokens abstracts
      rw [whole, show css.length = (characters css).length by simp [characters],
        List.take_length] at tokens
      exact tokens
  · exact invariant

/-- The translated scanner returns exactly the tokens `Design.parse` finds in
the same bytes, and never fails. -/
theorem scan_token_spans_refines (css : Slice U8) :
    theme.scan_token_spans css ⦃ spans =>
      spans.val.map (spanToken css) = parse (characters css) ⦄ := by
  unfold theme.scan_token_spans
  apply scan_token_spans_loop_refines
  simp [LoopInvariant, Bounded, abstractState, abstractMode, start]

/-- With the design's correctness: on any well-formed stylesheet, the Rust
scanner returns exactly its declarations (SPEC.md §6, §3). -/
theorem scan_token_spans_correct (css : Slice U8) (items : List Design.Item)
    (wellFormed : Design.WellFormed items) (rendered : characters css = Design.render items) :
    theme.scan_token_spans css ⦃ spans =>
      spans.val.map (spanToken css) = Design.declarations items ⦄ := by
  apply WP.spec_mono (scan_token_spans_refines css)
  intro spans refines
  rw [refines, rendered]
  exact Design.correct items wellFormed

#print axioms scan_step_refines
#print axioms scan_token_spans_loop_refines
#print axioms scan_token_spans_refines
#print axioms scan_token_spans_correct

end Velocity.Theme.Confidence
