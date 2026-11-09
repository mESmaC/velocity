module
public import AcpClient.Design
public import AcpClient.Generated.Funs
public section

/-!
# ACP client: confidence

Proof level: **code**. `Generated/` is the Charon + Aeneas translation of the pure modules this
feature adds to `crates/velocity-acp`:

```
powershell -File proofs/scripts/Export-Rust.ps1 -CratePath crates/velocity-acp -Feature AcpClient `
  -StartFrom crate::paths,crate::session_start,crate::terminal_output,crate::transcript
```

`-StartFrom` keeps the agent runner's modules (translated by `AgentRunner`) and the async
`client` edge out of the translation. Each theorem below shows that a translated Rust function
never fails and returns what the design in `Design.lean` returns, through the `lift*` maps
from the translated types to the design's types.

Hypotheses on inputs:
- `resolve_path_refines`: the worktree and requested path together are at least 3 bytes
  shorter than `usize::MAX`, so the resolved path (at most `w + r + 3` bytes,
  `resolvePath_cost`) fits in a `Vec`. Every real path does.
- `keep_first_by_name_refines` and the transcript theorems: `PartialEq` on names and keys is
  equality (`lawful`). The client compares `Option<&str>` server names and `Arc<str>` keys,
  whose `eq` is.
- `record_refines`: the entry list and each payload list have room for one more element, below
  `usize::MAX`, where `Vec::push` itself fails. `recordAll_refines` discharges this for every
  run of fewer than `usize::MAX` updates.

`recordAll_refines` lifts `record_refines` to any sequence of updates recorded from
`Transcript::new`, which is what the client does with each `session/update` notification. The
design's `transcript_*` theorems therefore hold of the Rust transcript.
-/

namespace Velocity.AcpClient

open Aeneas Aeneas.Std Result ControlFlow
open Generated

/-! ## From the translated types to the design's -/

@[expose] def liftBytes (bytes : List U8) : Bytes := bytes.map (·.val)

@[step]
theorem is_separator_refines (byte : U8) :
    paths.is_separator byte ⦃ result => result = isSeparator byte.val ⦄ := by
  unfold paths.is_separator
  simp only [paths.BACKSLASH, paths.SLASH, UScalar.eq_equiv]
  split <;> simp_all [WP.spec_ok, isSeparator, Bool.beq_eq_decide_eq]

@[step]
theorem is_between_refines (byte low high : U8) :
    paths.is_between byte low high ⦃ result =>
      result = (decide (low.val ≤ byte.val) && decide (byte.val ≤ high.val)) ⦄ := by
  unfold paths.is_between
  split <;> simp_all [WP.spec_ok, UScalar.le_equiv]

@[step]
theorem is_drive_letter_refines (byte : U8) :
    paths.is_drive_letter byte ⦃ result => result = isDriveLetter byte.val ⦄ := by
  unfold paths.is_drive_letter
  step with is_between_refines as ⟨upper, upperIs⟩
  split
  · simp_all [WP.spec_ok, isDriveLetter]
  · apply WP.spec_mono (is_between_refines byte _ _)
    intro result resultIs
    simp_all [isDriveLetter]

@[step]
theorem fold_case_refines (byte : U8) :
    paths.fold_case byte ⦃ result => result.val = foldCase byte.val ⦄ := by
  unfold paths.fold_case
  step with is_between_refines as ⟨upper, upperIs⟩
  split
  · step as ⟨folded, foldedIs⟩
    simp_all [foldCase]
  · simp_all [WP.spec_ok, foldCase]

@[step]
theorem to_backslash_refines (byte : U8) :
    paths.to_backslash byte ⦃ result => result.val = toBackslash byte.val ⦄ := by
  unfold paths.to_backslash
  simp only [paths.BACKSLASH, paths.SLASH, UScalar.eq_equiv]
  split <;> simp_all [WP.spec_ok, toBackslash, Bool.beq_eq_decide_eq]

@[expose] def liftShape (path : Bytes) : paths.PathShape → Shape
  | .Empty => .empty
  | .Absolute => .absolute
  | .RootRelative => .rootRelative
  | .DriveRelative => .driveRelative (path.headD 0) (path.drop 2)
  | .Relative => .relative

@[step]
theorem shape_refines (path : Slice U8) :
    paths.shape path ⦃ result => liftShape (liftBytes path.val) result = shape (liftBytes path.val) ⦄ := by
  unfold paths.shape
  rcases bytes : path.val with _ | ⟨first, _ | ⟨second, _ | ⟨third, rest⟩⟩⟩
  all_goals
    step*
    all_goals try simp_all [liftShape, liftBytes, shape, UScalar.eq_equiv, paths.COLON]
    all_goals scalar_tac

@[step]
theorem drive_of_refines (worktree : Slice U8) :
    paths.drive_of worktree ⦃ result =>
      result.map (·.val) = driveOf (liftBytes worktree.val) ⦄ := by
  unfold paths.drive_of
  rcases bytes : worktree.val with _ | ⟨first, _ | ⟨second, rest⟩⟩
  all_goals
    step*
    all_goals simp_all [liftBytes, driveOf, UScalar.eq_equiv, paths.COLON]

theorem anyM_equal_refines (bytes : List U8) (byte : U8) :
    List.anyM (core.cmp.PartialEqU8.eq byte) bytes ⦃ result =>
      result = (liftBytes bytes).contains byte.val ⦄ := by
  induction bytes with
  | nil => simp [liftBytes, pure, WP.spec_ok]
  | cons head tail tailRefines =>
    by_cases same : byte = head
    · subst same
      simp [liftBytes, pure, WP.spec_ok]
    · have differ : byte.val ≠ head.val := fun equal => same ((UScalar.eq_equiv byte head).mpr equal)
      simp only [List.anyM_cons]
      simpa [liftBytes, same, differ, Ne.symm differ, List.contains_cons] using tailRefines

@[step]
theorem windows_style_refines (worktree : Slice U8) :
    paths.windows_style worktree ⦃ result => result = windowsStyle (liftBytes worktree.val) ⦄ := by
  unfold paths.windows_style
  step as ⟨drive, driveIs⟩
  cases drive with
  | none =>
    apply WP.spec_mono (anyM_equal_refines worktree.val paths.BACKSLASH)
    intro result resultIs
    simp only [Option.map_none] at driveIs
    simp [windowsStyle, ← driveIs, resultIs, paths.BACKSLASH]
  | some letter =>
    simp only [Option.map_some] at driveIs
    simp [WP.spec_ok, windowsStyle, ← driveIs]

theorem liftBytes_drop_cons (bytes : List U8) (index : Nat) (inRange : index < bytes.length) :
    (liftBytes bytes).drop index = bytes[index].val :: (liftBytes bytes).drop (index + 1) := by
  unfold liftBytes
  rw [List.drop_eq_getElem_cons (by simpa using inRange)]
  simp

@[step]
theorem unc_root_end_loop_refines (worktree : Slice U8) (separators : I32) (index : Usize)
    (separatorsLow : 0 ≤ separators.val) (separatorsHigh : separators.val ≤ 1)
    (indexBound : index.val ≤ worktree.length) :
    paths.unc_root_end_loop worktree separators index ⦃ result =>
      result.val = uncRootEnd separators.val.toNat index.val
        ((liftBytes worktree.val).drop index.val) ⦄ := by
  unfold paths.unc_root_end_loop
  apply loop.spec_decr_nat (fun state => worktree.length - state.2.val)
    (fun state => 0 ≤ state.1.val ∧ state.1.val ≤ 1 ∧ state.2.val ≤ worktree.length ∧
      uncRootEnd state.1.val.toNat state.2.val ((liftBytes worktree.val).drop state.2.val) =
        uncRootEnd separators.val.toNat index.val ((liftBytes worktree.val).drop index.val))
  · rintro ⟨count, position⟩ ⟨countLow, countHigh, positionBound, invariant⟩
    unfold paths.unc_root_end_loop.body
    dsimp only
    split
    · rename_i inRange
      have inList : position.val < worktree.val.length := by scalar_tac
      rw [liftBytes_drop_cons worktree.val position.val inList] at invariant
      step as ⟨byte, byteIs⟩
      step as ⟨separator, separatorIs⟩
      split
      · rename_i isSeparatorByte
        split
        · rename_i lastSeparator
          have countOne : count.val = 1 := by scalar_tac
          simp only [WP.spec_ok]
          rw [← invariant]
          simp_all [uncRootEnd]
        · rename_i firstSeparator
          have countZero : count.val = 0 := by scalar_tac
          step as ⟨nextCount, nextCountIs⟩
          step as ⟨nextPosition, nextPositionIs⟩
          refine ⟨by scalar_tac, by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
          rw [← invariant]
          simp_all [uncRootEnd]
      · rename_i notSeparator
        step as ⟨nextPosition, nextPositionIs⟩
        refine ⟨by scalar_tac, by scalar_tac, by scalar_tac, ?_, by scalar_tac⟩
        rw [← invariant]
        simp_all [uncRootEnd]
    · rename_i atEnd
      have positionEnd : position.val = worktree.val.length := by scalar_tac
      simp only [WP.spec_ok]
      rw [← invariant, positionEnd]
      simp [liftBytes, uncRootEnd]
  · exact ⟨separatorsLow, separatorsHigh, indexBound, rfl⟩

@[step]
theorem root_length_refines (worktree : Slice U8) :
    paths.root_length worktree ⦃ result => result.val = rootLength (liftBytes worktree.val) ⦄ := by
  unfold paths.root_length
  rcases bytes : worktree.val with _ | ⟨first, _ | ⟨second, rest⟩⟩
  all_goals
    step*
    all_goals simp_all [liftBytes, rootLength, UScalar.eq_equiv, paths.COLON]

@[step]
theorem needs_separator_refines (worktree : Slice U8) :
    paths.needs_separator worktree ⦃ result =>
      result = needsSeparator (liftBytes worktree.val) ⦄ := by
  unfold paths.needs_separator
  step*
  · simp_all [liftBytes, needsSeparator]
  · have lastInRange : worktree.val.length - 1 < worktree.val.length := by scalar_tac
    simp_all [liftBytes, needsSeparator, List.getLast?_eq_getElem?]

@[step]
theorem separator_for_refines (worktree : Slice U8) :
    paths.separator_for worktree ⦃ result =>
      result.val = separatorFor (liftBytes worktree.val) ⦄ := by
  unfold paths.separator_for
  step*
  all_goals simp_all [separatorFor, paths.BACKSLASH, paths.SLASH]

/-- What `push_bytes` appends: `part`, with `/` turned into `\` when `windows` is set. -/
@[expose] def pushed (windows : Bool) (part : Bytes) : Bytes :=
  if windows then part.map toBackslash else part

theorem pushed_cons (windows : Bool) (byte : Nat) (rest : Bytes) :
    pushed windows (byte :: rest) =
      (if windows then toBackslash byte else byte) :: pushed windows rest := by
  cases windows <;> simp [pushed]

theorem pushed_nil (windows : Bool) : pushed windows [] = [] := by
  cases windows <;> simp [pushed]

theorem liftBytes_window_cons (bytes : List U8) (stop position : Nat)
    (positionInRange : position < stop) (stopBound : stop ≤ bytes.length) :
    ((liftBytes bytes).take stop).drop position =
      bytes[position].val :: ((liftBytes bytes).take stop).drop (position + 1) := by
  have inTake : position < ((liftBytes bytes).take stop).length := by
    simp [liftBytes]
    omega
  rw [List.drop_eq_getElem_cons inTake]
  simp [liftBytes]

@[step]
theorem push_bytes_loop_refines (out : alloc.vec.Vec U8) (bytes : Slice U8) (stop : Usize)
    (windows : Bool) (index : Usize)
    (indexBound : index.val ≤ stop.val) (stopBound : stop.val ≤ bytes.length)
    (room : out.val.length + (stop.val - index.val) ≤ Usize.max) :
    paths.push_bytes_loop out bytes stop windows index ⦃ result =>
      liftBytes result.val = liftBytes out.val ++
        pushed windows (((liftBytes bytes.val).take stop.val).drop index.val) ⦄ := by
  unfold paths.push_bytes_loop
  apply loop.spec_decr_nat (fun state => stop.val - state.2.val)
    (fun state => state.2.val ≤ stop.val ∧
      state.1.val.length + (stop.val - state.2.val) ≤ Usize.max ∧
      liftBytes state.1.val ++
          pushed windows (((liftBytes bytes.val).take stop.val).drop state.2.val) =
        liftBytes out.val ++
          pushed windows (((liftBytes bytes.val).take stop.val).drop index.val))
  · rintro ⟨written, position⟩ ⟨positionBound, writtenRoom, invariant⟩
    unfold paths.push_bytes_loop.body
    dsimp only
    split
    · rename_i inRange
      rw [liftBytes_window_cons bytes.val stop.val position.val inRange stopBound,
        pushed_cons] at invariant
      step as ⟨byte, byteIs⟩
      split
      · rename_i isWindows
        step as ⟨converted, convertedIs⟩
        step as ⟨nextWritten, nextWrittenIs⟩
        step as ⟨nextPosition, nextPositionIs⟩
        refine ⟨by scalar_tac, by simp [nextWrittenIs]; scalar_tac, ?_, by scalar_tac⟩
        rw [← invariant]
        simp_all [liftBytes]
      · rename_i notWindows
        step as ⟨nextWritten, nextWrittenIs⟩
        step as ⟨nextPosition, nextPositionIs⟩
        refine ⟨by scalar_tac, by simp [nextWrittenIs]; scalar_tac, ?_, by scalar_tac⟩
        rw [← invariant]
        simp_all [liftBytes]
    · rename_i atStop
      have positionStop : position.val = stop.val := by scalar_tac
      simp only [WP.spec_ok]
      rw [← invariant, positionStop]
      simp [liftBytes, pushed_nil]
  · exact ⟨indexBound, room, rfl⟩

theorem uncRootEnd_le (separators index : Nat) (rest : Bytes) :
    uncRootEnd separators index rest ≤ index + rest.length := by
  induction rest generalizing separators index with
  | nil => simp [uncRootEnd]
  | cons byte rest restBound =>
    simp only [uncRootEnd]
    have nextBound := restBound (separators + 1) (index + 1)
    have sameBound := restBound separators (index + 1)
    split
    · split
      · simp
      · simp only [List.length_cons]
        omega
    · simp only [List.length_cons]
      omega

theorem rootLength_le (worktree : Bytes) : rootLength worktree ≤ worktree.length := by
  match worktree with
  | [] => simp [rootLength]
  | [_] => simp [rootLength]
  | first :: second :: rest =>
    have := uncRootEnd_le 0 2 rest
    simp only [rootLength, List.length_cons]
    split
    · omega
    · split <;> omega

theorem normalize_eq_pushed (worktree part : Bytes) :
    normalize worktree part = pushed (windowsStyle worktree) part := by
  simp [normalize, pushed]

theorem take_length_map (bytes : List U8) :
    (bytes.map (·.val)).take bytes.length = bytes.map (·.val) := by
  simp

@[step]
theorem join_relative_refines (worktree part : Slice U8) (start : Usize)
    (startBound : start.val ≤ part.length)
    (room : worktree.length + 1 + part.length ≤ Usize.max) :
    paths.join_relative worktree part start ⦃ result =>
      liftBytes result.val =
        joinRelative (liftBytes worktree.val) ((liftBytes part.val).drop start.val) ⦄ := by
  unfold paths.join_relative
  step as ⟨copy, copyIs⟩
  have copyVal : copy.val = worktree.val := by rw [copyIs]; rfl
  step as ⟨needs, needsIs⟩
  split
  · rename_i needsIt
    step as ⟨separator, separatorIs⟩
    have copyRoom : copy.val.length < Usize.max := by rw [copyVal]; scalar_tac
    step with alloc.vec.Vec.push_spec as ⟨withSeparator, withSeparatorIs⟩
    step as ⟨windows, windowsIs⟩
    have pushRoom : withSeparator.val.length + (part.len.val - start.val) ≤ Usize.max := by
      rw [withSeparatorIs, copyVal]
      simp only [List.length_append, List.length_singleton, Slice.len_val]
      scalar_tac
    step as ⟨joined, joinedIs⟩
    rw [joinedIs, withSeparatorIs, copyVal]
    simp_all [joinRelative, normalize_eq_pushed, liftBytes, take_length_map]
  · rename_i noSeparator
    step as ⟨windows, windowsIs⟩
    step as ⟨joined, joinedIs⟩
    rw [joinedIs, copyVal]
    simp_all [joinRelative, normalize_eq_pushed, liftBytes, take_length_map]

@[step]
theorem same_drive_refines (drive : Option U8) (letter : U8) :
    paths.same_drive drive letter ⦃ result =>
      result = decide (drive.map (·.val) = some (foldCase letter.val)) ⦄ := by
  unfold paths.same_drive
  cases drive with
  | none => simp [WP.spec_ok]
  | some drive =>
    step as ⟨folded, foldedIs⟩
    simp [UScalar.eq_equiv, foldedIs]

theorem take_len_liftBytes (bytes : Slice U8) :
    (liftBytes bytes.val).take bytes.len.val = liftBytes bytes.val := by
  simp [liftBytes]

theorem pushed_length (windows : Bool) (part : Bytes) :
    (pushed windows part).length = part.length := by
  cases windows <;> simp [pushed]

/-- `resolve_path` refines the design's `resolvePath`, for every worktree and requested path
whose resolved path fits in memory: the result is at most `w + r + 3` bytes
(`resolvePath_cost`), so `room` only excludes paths within 3 bytes of `usize::MAX`. -/
theorem resolve_path_refines (worktree requested : Slice U8)
    (room : worktree.length + requested.length + 3 ≤ Usize.max) :
    paths.resolve_path worktree requested ⦃ result =>
      liftBytes result.val = resolvePath (liftBytes worktree.val) (liftBytes requested.val) ⦄ := by
  unfold paths.resolve_path
  step as ⟨requestedShape, shapeIs⟩
  cases requestedShape with
  | Empty =>
    simp only [liftShape] at shapeIs
    step as ⟨copy, copyIs⟩
    have copyVal : copy.val = worktree.val := by rw [copyIs]; rfl
    simp [resolvePath, ← shapeIs, copyVal]
  | Absolute =>
    simp only [liftShape] at shapeIs
    step as ⟨copy, copyIs⟩
    have copyVal : copy.val = requested.val := by rw [copyIs]; rfl
    simp [resolvePath, ← shapeIs, copyVal]
  | RootRelative =>
    simp only [liftShape] at shapeIs
    step as ⟨rootEnd, rootEndIs⟩
    have rootBound : rootEnd.val ≤ worktree.val.length := by
      have := rootLength_le (liftBytes worktree.val)
      simp only [liftBytes, List.length_map] at this
      rw [rootEndIs]
      exact this
    step as ⟨root, rootIs⟩
    have rootSize : root.val.length = rootEnd.val := by
      have := congrArg List.length rootIs
      simp only [liftBytes, List.length_map, List.length_append, pushed_length, List.drop_zero,
        List.length_take] at this
      have newEmpty : (alloc.vec.Vec.new U8).val = [] := rfl
      simp only [newEmpty, List.length_nil, Nat.zero_add] at this
      omega
    step as ⟨windows, windowsIs⟩
    step as ⟨joined, joinedIs⟩
    rw [joinedIs, rootIs, take_len_liftBytes]
    simp only [resolvePath]
    rw [← shapeIs]
    simp [pushed, normalize_eq_pushed, windowsIs, rootEndIs, liftBytes]
  | DriveRelative =>
    simp only [liftShape] at shapeIs
    rcases bytes : requested.val with _ | ⟨first, _ | ⟨second, rest⟩⟩
    · simp [bytes, liftBytes, shape] at shapeIs
    · simp only [bytes, liftBytes, List.map_cons, List.map_nil, shape] at shapeIs
      split at shapeIs <;> simp at shapeIs
    have requestedLong : 2 ≤ requested.val.length := by simp [bytes]
    step as ⟨drive, driveIs⟩
    step as ⟨worktreeDrive, worktreeDriveIs⟩
    step as ⟨same, sameIs⟩
    simp only [bytes, liftBytes, List.map_cons, List.headD_cons, List.drop_succ_cons,
      List.drop_zero] at shapeIs
    have driveVal : drive.val = first.val := by simp [driveIs, bytes]
    split
    · rename_i sameDrive
      step as ⟨joined, joinedIs⟩
      rw [joinedIs]
      simp only [resolvePath, bytes, liftBytes, List.map_cons]
      rw [← shapeIs]
      clear shapeIs
      simp_all [liftBytes]
    · rename_i otherDrive
      simp only [alloc.vec.FromVecArray.from]
      step as ⟨joined, joinedIs⟩
      rw [joinedIs, take_len_liftBytes]
      simp only [resolvePath, bytes, liftBytes, List.map_cons]
      rw [← shapeIs]
      clear shapeIs
      simp_all [liftBytes, pushed, paths.COLON, paths.BACKSLASH]
  | Relative =>
    simp only [liftShape] at shapeIs
    step as ⟨joined, joinedIs⟩
    rw [joinedIs]
    simp [resolvePath, ← shapeIs]

/-! ## Terminal output -/

@[step]
theorem is_continuation_refines (byte : U8) :
    terminal_output.is_continuation byte ⦃ result => result = isContinuation byte.val ⦄ := by
  unfold terminal_output.is_continuation
  split <;> simp_all [WP.spec_ok, isContinuation, UScalar.le_equiv, UScalar.lt_equiv]

@[step]
theorem retained_output_start_loop_refines (output : Slice U8) (start : Usize)
    (startBound : start.val ≤ output.length) :
    terminal_output.retained_output_start_loop output start ⦃ result =>
      result.val = start.val +
        (((liftBytes output.val).drop start.val).takeWhile isContinuation).length ⦄ := by
  unfold terminal_output.retained_output_start_loop
  apply loop.spec_decr_nat (fun position => output.length - position.val)
    (fun position => position.val ≤ output.length ∧
      position.val + (((liftBytes output.val).drop position.val).takeWhile isContinuation).length =
        start.val + (((liftBytes output.val).drop start.val).takeWhile isContinuation).length)
  · rintro position ⟨positionBound, invariant⟩
    unfold terminal_output.retained_output_start_loop.body
    dsimp only
    split
    · rename_i inRange
      have inList : position.val < output.val.length := by scalar_tac
      rw [liftBytes_drop_cons output.val position.val inList] at invariant
      step as ⟨byte, byteIs⟩
      step as ⟨continuation, continuationIs⟩
      split
      · rename_i isContinuationByte
        step as ⟨next, nextIs⟩
        refine ⟨by scalar_tac, ?_, by scalar_tac⟩
        rw [← invariant]
        simp_all only [List.takeWhile_cons, ↓reduceIte, List.length_cons]
        omega
      · rename_i notContinuation
        simp only [WP.spec_ok]
        rw [← invariant]
        simp_all
    · rename_i atEnd
      have positionEnd : position.val = output.val.length := by scalar_tac
      simp only [WP.spec_ok]
      rw [← invariant, positionEnd]
      simp [liftBytes]
  · exact ⟨startBound, rfl⟩

theorem retained_output_start_refines (output : Slice U8) (limit : Usize) :
    terminal_output.retained_output_start output limit ⦃ result =>
      result.val = retainedStart (liftBytes output.val) limit.val ⦄ := by
  unfold terminal_output.retained_output_start
  dsimp only
  split
  · rename_i fits
    simp only [WP.spec_ok]
    have fitsVal : output.val.length ≤ limit.val := by scalar_tac
    simp [retainedStart, liftBytes, fitsVal]
  · rename_i overflows
    step as ⟨start, startIs⟩
    step as ⟨retained, retainedIs⟩
    have overflowsVal : ¬ output.val.length ≤ limit.val := by scalar_tac
    rw [retainedIs, startIs]
    simp [retainedStart, liftBytes, overflowsVal]

/-! ## Session start -/

section SessionStart

variable {Name : Type} [DecidableEq Name]

theorem appears_before_loop_refines (equality : core.cmp.PartialEq Name Name)
    (lawful : ∀ first second, equality.eq first second = ok (decide (first = second)))
    (names : Slice Name) (index earlier : Usize) (indexInRange : index.val < names.length)
    (earlierBound : earlier.val ≤ index.val)
    (notEarlier : names.val[index.val] ∉ names.val.take earlier.val) :
    session_start.appears_before_loop equality names index earlier ⦃ result =>
      result = decide (names.val[index.val] ∈ names.val.take index.val) ⦄ := by
  unfold session_start.appears_before_loop
  apply loop.spec_decr_nat (fun position => index.val - position.val)
    (fun position => position.val ≤ index.val ∧
      names.val[index.val] ∉ names.val.take position.val)
  · rintro position ⟨positionBound, absent⟩
    unfold session_start.appears_before_loop.body
    split
    · rename_i before
      step as ⟨candidate, candidateIs⟩
      step as ⟨current, currentIs⟩
      rw [lawful]
      simp only [bind_ok]
      split
      · rename_i same
        simp only [WP.spec_ok]
        have found : names.val[index.val] ∈ names.val.take index.val := by
          have sameNames : candidate = current := by simpa using same
          rw [List.mem_take_iff_getElem]
          exact ⟨position.val, by simp; scalar_tac, by simp_all⟩
        simp [found]
      · rename_i differ
        step as ⟨next, nextIs⟩
        refine ⟨by scalar_tac, ?_, by scalar_tac⟩
        have positionInList : position.val < names.val.length := by scalar_tac
        rw [nextIs, List.take_add_one, List.getElem?_eq_getElem positionInList]
        have differNames : names.val[position.val] ≠ names.val[index.val] := by
          simp_all
        simp only [Option.toList_some, List.mem_append, List.mem_singleton, not_or]
        exact ⟨absent, Ne.symm differNames⟩
    · rename_i reached
      have positionIndex : position.val = index.val := by scalar_tac
      simp only [WP.spec_ok]
      rw [positionIndex] at absent
      simp [absent]
  · exact ⟨earlierBound, notEarlier⟩

theorem keep_first_by_name_refines (equality : core.cmp.PartialEq Name Name)
    (lawful : ∀ first second, equality.eq first second = ok (decide (first = second)))
    (names : Slice Name) :
    session_start.keep_first_by_name equality names ⦃ result =>
      result.val = keepFirstByName names.val ⦄ := by
  unfold session_start.keep_first_by_name session_start.keep_first_by_name_loop
  apply loop.spec_decr_nat (fun state => names.length - state.2.val)
    (fun state => state.2.val ≤ names.length ∧ state.1.val.length = state.2.val ∧
      state.1.val ++ keepFrom (names.val.take state.2.val) (names.val.drop state.2.val) =
        keepFirstByName names.val)
  · rintro ⟨keep, index⟩ ⟨indexBound, keepLength, invariant⟩
    unfold session_start.keep_first_by_name_loop.body
    dsimp only
    split
    · rename_i inRange
      have inList : index.val < names.val.length := by scalar_tac
      have appears := appears_before_loop_refines equality lawful names index 0#usize
        inRange (by simp) (by simp)
      step with appears as ⟨earlier, earlierIs⟩
      step as ⟨nextKeep, nextKeepIs⟩
      step as ⟨next, nextIs⟩
      refine ⟨by scalar_tac, by simp [nextKeepIs, keepLength, nextIs], ?_, by scalar_tac⟩
      rw [← invariant, List.drop_eq_getElem_cons inList, nextIs, nextKeepIs,
        List.take_add_one, List.getElem?_eq_getElem inList]
      simp only [keepFrom, earlierIs]
      simp
      exact Iff.rfl
    · rename_i atEnd
      have indexEnd : index.val = names.val.length := by scalar_tac
      simp only [WP.spec_ok]
      rw [← invariant, indexEnd]
      simp [keepFrom]
  · refine ⟨by simp, by simp, ?_⟩
    simp [keepFirstByName]

end SessionStart

theorem mode_to_select_refines (bypass current : Option Usize) :
    session_start.mode_to_select bypass current ⦃ result =>
      result.map (·.val) = modeToSelect (bypass.map (·.val)) (current.map (·.val)) ⦄ := by
  unfold session_start.mode_to_select session_start.needs_no_change session_start.is_mode
  cases bypass with
  | none => simp [WP.spec_ok, modeToSelect]
  | some mode =>
    cases current with
    | none => simp [WP.spec_ok, modeToSelect]
    | some current =>
      by_cases same : current = mode
      · simp [WP.spec_ok, modeToSelect, same]
      · have differ : current.val ≠ mode.val := fun equal =>
          same ((UScalar.eq_equiv current mode).mpr equal)
        simp [WP.spec_ok, modeToSelect, same, differ]

/-! ## Transcript -/

section TranscriptRefinement

variable {Key Payload : Type} [DecidableEq Key]

@[expose] def liftMessage : transcript.MessageKey Key → Option Key
  | .Unnamed => none
  | .Named key => some key

@[expose] def liftClass : transcript.EntryClass Key → EntryClass Key
  | .UserMessage message => .userMessage (liftMessage message)
  | .AgentMessage message => .agentMessage (liftMessage message)
  | .AgentThought message => .agentThought (liftMessage message)
  | .ToolCall call => .toolCall call
  | .Plan => .plan

@[expose] def liftEntry (entry : transcript.Entry Key Payload) : Entry Key Payload :=
  ⟨liftClass entry.class, entry.payloads.val⟩

@[expose] def liftPlacement : transcript.Placement → Placement
  | .Appended index => .appended index.val
  | .Added index => .added index.val

variable (equality : core.cmp.PartialEq Key Key)
  (lawful : ∀ first second, equality.eq first second = ok (decide (first = second)))
include lawful

theorem same_message_refines (first second : transcript.MessageKey Key) :
    transcript.same_message equality first second ⦃ result =>
      result = decide (liftMessage first = liftMessage second) ⦄ := by
  unfold transcript.same_message
  cases first <;> cases second <;> simp [WP.spec_ok, liftMessage, lawful]

theorem same_class_refines (first second : transcript.EntryClass Key) :
    transcript.same_class equality first second ⦃ result =>
      result = decide (liftClass first = liftClass second) ⦄ := by
  unfold transcript.same_class
  cases first <;> cases second
  all_goals first
    | (apply WP.spec_mono (same_message_refines equality lawful _ _)
       intro result resultIs
       simp [liftClass, resultIs])
    | simp [WP.spec_ok, liftClass, lawful]

theorem find_class_loop_refines (entries : Slice (transcript.Entry Key Payload))
    (entryClass : transcript.EntryClass Key) :
    transcript.find_class_loop equality entries entryClass 0#usize ⦃ result =>
      result.map (·.val) = findClass (liftClass entryClass) (entries.val.map liftEntry) ⦄ := by
  unfold transcript.find_class_loop
  apply loop.spec_decr_nat (fun position => entries.length - position.val)
    (fun position => position.val ≤ entries.length ∧
      (findClass (liftClass entryClass) ((entries.val.map liftEntry).drop position.val)).map
          (· + position.val) =
        findClass (liftClass entryClass) (entries.val.map liftEntry))
  · rintro position ⟨positionBound, invariant⟩
    unfold transcript.find_class_loop.body
    dsimp only
    split
    · rename_i inRange
      have inList : position.val < entries.val.length := by scalar_tac
      rw [List.drop_eq_getElem_cons (by simpa using inList)] at invariant
      step as ⟨entry, entryIs⟩
      step with same_class_refines equality lawful as ⟨same, sameIs⟩
      split
      · rename_i isSame
        simp only [WP.spec_ok]
        rw [← invariant]
        simp_all [findClass, liftEntry]
      · rename_i notSame
        step as ⟨next, nextIs⟩
        refine ⟨by scalar_tac, ?_, by scalar_tac⟩
        rw [← invariant, nextIs]
        simp_all [findClass, liftEntry, Option.map_map, Function.comp_def, Nat.add_assoc,
          Nat.add_comm 1]
    · rename_i atEnd
      have positionEnd : position.val = entries.val.length := by scalar_tac
      simp only [WP.spec_ok]
      rw [← invariant, positionEnd]
      simp [findClass]
  · exact ⟨by simp, by simp⟩

theorem entry_for_refines (entries : Slice (transcript.Entry Key Payload))
    (entryClass : transcript.EntryClass Key) :
    transcript.entry_for equality entries entryClass ⦃ result =>
      result.map (·.val) = entryFor (entries.val.map liftEntry) (liftClass entryClass) ⦄ := by
  unfold transcript.entry_for transcript.is_tool_call transcript.find_class
  cases toolCall : entryClass with
  | ToolCall call =>
    simp only [bind_ok, ↓reduceIte]
    apply WP.spec_mono (find_class_loop_refines equality lawful entries _)
    intro result resultIs
    simp [entryFor, resultIs, liftClass, EntryClass.isToolCall]
  | Plan | UserMessage _ | AgentMessage _ | AgentThought _ =>
    simp only [bind_ok, Bool.false_eq_true, ↓reduceIte]
    step as ⟨empty, emptyIs⟩
    split
    · simp_all [WP.spec_ok, entryFor, liftClass, EntryClass.isToolCall]
    · step as ⟨last, lastIs⟩
      step as ⟨entry, entryIs⟩
      step with same_class_refines equality lawful as ⟨same, sameIs⟩
      have lastInRange : entries.val.length - 1 < entries.val.length := by scalar_tac
      split
      all_goals
        simp_all [WP.spec_ok, entryFor, liftClass, EntryClass.isToolCall,
          List.getLast?_eq_getElem?, liftEntry]

end TranscriptRefinement

section TranscriptRecord

variable {Key Payload : Type} [DecidableEq Key]

omit [DecidableEq Key] in
theorem deref_val (entries : alloc.vec.Vec (transcript.Entry Key Payload)) :
    (alloc.vec.Vec.deref entries).val = entries.val := by
  unfold alloc.vec.Vec.deref
  simp

theorem entryFor_lt (entries : List (Entry Key Payload)) (entryClass : EntryClass Key)
    (index : Nat) (found : entryFor entries entryClass = some index) :
    index < entries.length := by
  simp only [entryFor] at found
  split at found
  · exact findClass_lt entryClass entries index found
  · split at found
    · rename_i last lastIs
      split at found
      · simp only [Option.some.injEq] at found
        have nonEmpty : entries ≠ [] := by
          intro empty
          simp [empty] at lastIs
        have := List.length_pos_iff.mpr nonEmpty
        omega
      · simp at found
    · simp at found

omit [DecidableEq Key] in
theorem appendAt_eq_set (payload : Payload) (entries : List (Entry Key Payload)) (index : Nat)
    (inRange : index < entries.length) :
    appendAt payload index entries =
      entries.set index
        { entries[index] with payloads := entries[index].payloads ++ [payload] } := by
  induction entries generalizing index with
  | nil => simp at inRange
  | cons entry rest restIs =>
    cases index with
    | zero => simp [appendAt]
    | succ index =>
      simp only [List.length_cons, Nat.add_lt_add_iff_right] at inRange
      simp [appendAt, restIs index inRange]

variable (equality : core.cmp.PartialEq Key Key)
  (lawful : ∀ first second, equality.eq first second = ok (decide (first = second)))
include lawful

/-- `Transcript::record` refines the design's `record`, for every transcript whose entry list
and payload lists have room for one more element (`Vec::push` needs it). -/
theorem record_refines (current : transcript.Transcript Key Payload)
    (entryClass : transcript.EntryClass Key) (payload : Payload)
    (room : current.entries.val.length < Usize.max)
    (payloadRoom : ∀ entry ∈ current.entries.val, entry.payloads.val.length < Usize.max) :
    transcript.Transcript.record equality current entryClass payload ⦃ placement next =>
      next.entries.val.map liftEntry =
          (record (current.entries.val.map liftEntry) (liftClass entryClass) payload).1 ∧
        liftPlacement placement =
          (record (current.entries.val.map liftEntry) (liftClass entryClass) payload).2 ⦄ := by
  unfold transcript.Transcript.record
  step with entry_for_refines equality lawful as ⟨found, foundIs⟩
  rw [deref_val] at foundIs
  cases found with
  | none =>
    simp only [Option.map_none] at foundIs
    simp only [alloc.vec.FromVecArray.from, bind_ok]
    step as ⟨extended, extendedIs⟩
    simp only [record, ← foundIs]
    simp [extendedIs, liftEntry, liftPlacement]
  | some index =>
    simp only [Option.map_some] at foundIs
    have inRange : index.val < current.entries.val.length := by
      have := entryFor_lt _ _ _ foundIs.symm
      simpa using this
    step as ⟨entry, back, entryIs, backIs⟩
    have entryRoom : entry.payloads.val.length < Usize.max :=
      payloadRoom entry (by rw [entryIs]; exact List.getElem_mem inRange)
    step as ⟨extended, extendedIs⟩
    simp only [record, ← foundIs]
    have liftedInRange : index.val < (current.entries.val.map liftEntry).length := by
      simpa using inRange
    rw [appendAt_eq_set payload _ index.val liftedInRange]
    simp [backIs, extendedIs, entryIs, liftEntry, liftPlacement, List.map_set]

omit lawful in
theorem record_size (entries : List (Entry Key Payload)) (entryClass : EntryClass Key)
    (payload : Payload) (bound : Nat) (entriesBound : entries.length ≤ bound)
    (payloadsBound : ∀ entry ∈ entries, entry.payloads.length ≤ bound) :
    (record entries entryClass payload).1.length ≤ bound + 1 ∧
      ∀ entry ∈ (record entries entryClass payload).1, entry.payloads.length ≤ bound + 1 := by
  simp only [record]
  split
  · rename_i index found
    have inRange := entryFor_lt entries entryClass index found
    rw [appendAt_eq_set payload entries index inRange]
    refine ⟨by simp only [List.length_set]; omega, ?_⟩
    intro entry member
    rcases List.mem_or_eq_of_mem_set member with old | new
    · have := payloadsBound entry old
      omega
    · rw [new]
      have := payloadsBound entries[index] (List.getElem_mem inRange)
      simp only [List.length_append, List.length_singleton]
      omega
  · refine ⟨by simp only [List.length_append, List.length_singleton]; omega, ?_⟩
    intro entry member
    rcases List.mem_append.mp member with old | new
    · have := payloadsBound entry old
      omega
    · simp only [List.mem_singleton] at new
      rw [new]
      simp

/-- The Rust side of a run: `Transcript::record` applied to each update in turn, as the ACP
client does with each `session/update` notification. -/
@[expose] def recordAll (current : transcript.Transcript Key Payload) :
    List (transcript.EntryClass Key × Payload) → Result (transcript.Transcript Key Payload)
  | [] => ok current
  | (entryClass, payload) :: rest => do
      let (_, next) ← transcript.Transcript.record equality current entryClass payload
      recordAll next rest

theorem recordAll_refines_from (updates : List (transcript.EntryClass Key × Payload))
    (current : transcript.Transcript Key Payload) (bound : Nat)
    (entriesBound : current.entries.val.length ≤ bound)
    (payloadsBound : ∀ entry ∈ current.entries.val, entry.payloads.val.length ≤ bound)
    (room : bound + updates.length < Usize.max) :
    recordAll equality current updates ⦃ result =>
      result.entries.val.map liftEntry =
        (updates.map fun update => (liftClass update.1, update.2)).foldl
          (fun entries update => (record entries update.1 update.2).1)
          (current.entries.val.map liftEntry) ⦄ := by
  induction updates generalizing current bound with
  | nil => simp [recordAll, WP.spec_ok]
  | cons update rest restRefines =>
    obtain ⟨entryClass, payload⟩ := update
    simp only [recordAll]
    simp only [List.length_cons] at room
    apply WP.spec_bind (record_refines equality lawful current entryClass payload
      (by omega) (fun entry member => by have := payloadsBound entry member; omega))
    rintro ⟨placement, next⟩ ⟨nextIs, _⟩
    have size := record_size (current.entries.val.map liftEntry) (liftClass entryClass) payload
      bound (by simpa using entriesBound)
      (by
        intro entry member
        obtain ⟨original, originalMember, lifted⟩ := List.mem_map.mp member
        rw [← lifted]
        exact payloadsBound original originalMember)
    rw [← nextIs] at size
    apply WP.spec_mono (restRefines next (bound + 1)
      (by simpa using size.1)
      (fun entry member => size.2 (liftEntry entry) (List.mem_map_of_mem member))
      (by omega))
    intro result resultIs
    rw [resultIs, nextIs]
    simp

/-- Every run of `Transcript::record` from an empty transcript builds the design's
`transcript` of the same updates, for runs of fewer than `usize::MAX` updates. The design's
`transcript_*` theorems therefore hold of the Rust transcript. -/
theorem recordAll_refines (updates : List (transcript.EntryClass Key × Payload))
    (room : updates.length < Usize.max) :
    (do
      let empty ← transcript.Transcript.new Key Payload
      recordAll equality empty updates) ⦃ result =>
      result.entries.val.map liftEntry =
        AcpClient.transcript (updates.map fun update => (liftClass update.1, update.2)) ⦄ := by
  have newEmpty : (alloc.vec.Vec.new (transcript.Entry Key Payload)).val = [] := rfl
  simp only [transcript.Transcript.new, bind_ok]
  apply WP.spec_mono (recordAll_refines_from equality lawful updates _ 0
    (by simp [newEmpty]) (by simp [newEmpty]) (by omega))
  intro result resultIs
  rw [resultIs, newEmpty]
  simp [AcpClient.transcript]

end TranscriptRecord

/-! ## Axiom audit -/

#print axioms is_separator_refines
#print axioms is_between_refines
#print axioms is_drive_letter_refines
#print axioms fold_case_refines
#print axioms to_backslash_refines
#print axioms shape_refines
#print axioms drive_of_refines
#print axioms windows_style_refines
#print axioms unc_root_end_loop_refines
#print axioms root_length_refines
#print axioms needs_separator_refines
#print axioms separator_for_refines
#print axioms push_bytes_loop_refines
#print axioms join_relative_refines
#print axioms same_drive_refines
#print axioms resolve_path_refines
#print axioms is_continuation_refines
#print axioms retained_output_start_loop_refines
#print axioms retained_output_start_refines
#print axioms appears_before_loop_refines
#print axioms keep_first_by_name_refines
#print axioms mode_to_select_refines
#print axioms same_message_refines
#print axioms same_class_refines
#print axioms find_class_loop_refines
#print axioms entry_for_refines
#print axioms record_refines
#print axioms recordAll_refines

end Velocity.AcpClient
