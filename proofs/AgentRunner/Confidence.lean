module
public import AgentRunner.Design
public import AgentRunner.Generated.Funs
public section

/-!
# Agent runner: confidence

Proof level: **code**. `Generated/` is the Charon + Aeneas translation of
`crates/velocity-acp` (`proofs/scripts/Export-Rust.ps1 -CratePath crates/velocity-acp
-Feature AgentRunner`). Each theorem below shows that a translated Rust function never fails
and returns what the design in `Design.lean` returns, through the `lift*` maps from the
translated types to the design's types.

The only hypothesis on inputs is in `step_refines` and `run_refines`: the queue of messages
waiting for one turn has room for the messages being sent, below `Usize.max`, the limit at
which Rust's `Vec::push` itself fails. No real thread queues 2^64 messages in one turn.

`run_refines` lifts `step_refines` to any sequence of inputs fed to `Steering::step` in turn,
which is what the ACP client does. The design theorems over runs (`correct`, `cost_bound`,
`optimal`) therefore hold of the Rust code.
-/

namespace Velocity.AgentRunner

open Aeneas Aeneas.Std Result ControlFlow
open Generated

variable {Message Settings : Type}

/-! ## From the translated types to the design's -/

@[expose] def liftProvider : run_settings.Provider → Provider
  | .Anthropic => .anthropic
  | .Codex => .codex
  | .Antigravity => .antigravity
  | .KimiCode => .kimiCode

@[expose] def liftEffort : run_settings.Effort → Effort
  | .Low => .low
  | .Medium => .medium
  | .High => .high
  | .Max => .max

@[expose] def liftSpeed : run_settings.Speed → Speed
  | .Regular => .regular
  | .Fast => .fast

@[expose] def liftRunSettings {Model : Type} (settings : run_settings.RunSettings Model) :
    RunSettings Model :=
  { provider := liftProvider settings.provider, model := settings.model,
    effort := liftEffort settings.effort, speed := liftSpeed settings.speed }

@[expose] def liftPermission : permissions.PermissionKind → PermissionKind
  | .AllowOnce => .allowOnce
  | .AllowAlways => .allowAlways
  | .RejectOnce => .rejectOnce
  | .RejectAlways => .rejectAlways

@[expose] def liftMode : permissions.SessionModeKind → SessionModeKind
  | .Bypass => .bypass
  | .Other => .other

@[expose] def liftSteering (runner : steering.Steering Message Settings) :
    Steering Message Settings :=
  { busy := runner.busy, queue := runner.queue.val, settings := runner.settings }

@[expose] def liftInput : steering.SteeringInput Message Settings → Input Message Settings
  | .Send message => .send message
  | .TurnEnded => .turnEnded
  | .Interrupt => .interrupt
  | .ChangeSettings settings => .changeSettings settings

@[expose] def liftEffect : steering.SteeringEffect Message → Effect Message
  | .Prompt messages => .prompt messages.val
  | .Queued => .queued
  | .Cancel => .cancel
  | .Nothing => .nothing

/-! ## Run settings -/

theorem effective_speed_refines (requested : run_settings.Speed) (hasFastMode : Bool) :
    run_settings.effective_speed requested hasFastMode ⦃ speed =>
      liftSpeed speed = effectiveSpeed (liftSpeed requested) hasFastMode ⦄ := by
  cases requested <;> cases hasFastMode <;>
    simp [run_settings.effective_speed, effectiveSpeed, liftSpeed]

theorem settings_effective_speed_refines {Model : Type}
    (settings : run_settings.RunSettings Model) (hasFastMode : Bool) :
    run_settings.RunSettings.effective_speed settings hasFastMode ⦃ speed =>
      liftSpeed speed = effectiveSpeed (liftRunSettings settings).speed hasFastMode ⦄ := by
  simpa [run_settings.RunSettings.effective_speed, liftRunSettings] using
    effective_speed_refines settings.speed hasFastMode

theorem with_model_refines {Model : Type} (settings : run_settings.RunSettings Model)
    (model : Model) :
    run_settings.RunSettings.with_model settings model ⦃ changed =>
      liftRunSettings changed = (liftRunSettings settings).withModel model ⦄ := by
  simp [run_settings.RunSettings.with_model, liftRunSettings, RunSettings.withModel]

/-! ## Permissions -/

private theorem drop_cons_getElem {α : Type} (list : List α) (index : Nat)
    (bound : index < list.length) :
    list.drop index = list[index] :: list.drop (index + 1) := by
  rw [List.drop_eq_getElem_cons bound]

theorem choose_permission_refines (options : Slice permissions.PermissionKind) :
    permissions.choose_permission options ⦃ chosen =>
      chosen.map (·.val) = choosePermission (options.val.map liftPermission) ⦄ := by
  unfold permissions.choose_permission permissions.choose_permission_loop
  apply loop.spec_decr_nat (fun state => options.length - state.2.val)
    (fun state => state.2.val ≤ options.length ∧
      choosePermissionFrom state.2.val (state.1.map (·.val))
          ((options.val.map liftPermission).drop state.2.val) =
        choosePermission (options.val.map liftPermission))
  · rintro ⟨fallback, index⟩ ⟨bound, invariant⟩
    unfold permissions.choose_permission_loop.body
    dsimp only
    split
    · rename_i inRange
      have inList : index.val < options.length := by scalar_tac
      step as ⟨kind, kindIsAt⟩
      have dropped := drop_cons_getElem (options.val.map liftPermission) index.val
        (by simpa using inList)
      rw [dropped] at invariant
      simp only [List.getElem_map] at invariant
      rw [← kindIsAt] at invariant
      cases kind with
      | AllowAlways =>
        dsimp only
        simp only [WP.spec_ok]
        simpa [liftPermission, choosePermissionFrom] using invariant
      | AllowOnce =>
        dsimp only
        unfold core.option.Option.is_none
        cases fallback with
        | none =>
          simp only [Option.isNone_none, ↓reduceIte]
          step as ⟨next, nextIs⟩
          refine ⟨by scalar_tac, ?_, by scalar_tac⟩
          rw [nextIs, ← invariant]
          simp [liftPermission, choosePermissionFrom]
        | some earlier =>
          simp only [Option.isNone_some, Bool.false_eq_true, ↓reduceIte]
          step as ⟨next, nextIs⟩
          refine ⟨by scalar_tac, ?_, by scalar_tac⟩
          rw [nextIs, ← invariant]
          simp [liftPermission, choosePermissionFrom]
      | RejectOnce =>
        dsimp only
        step as ⟨next, nextIs⟩
        refine ⟨by scalar_tac, ?_, by scalar_tac⟩
        rw [nextIs, ← invariant]
        simp [liftPermission, choosePermissionFrom]
      | RejectAlways =>
        dsimp only
        step as ⟨next, nextIs⟩
        refine ⟨by scalar_tac, ?_, by scalar_tac⟩
        rw [nextIs, ← invariant]
        simp [liftPermission, choosePermissionFrom]
    · rename_i outOfRange
      have atEnd : options.length ≤ index.val := by scalar_tac
      simp only [WP.spec_ok]
      rw [← invariant, List.drop_eq_nil_of_le (by simpa using atEnd)]
      rfl
  · simp [choosePermission]

theorem choose_session_mode_refines (modes : Slice permissions.SessionModeKind) :
    permissions.choose_session_mode modes ⦃ chosen =>
      chosen.map (·.val) = chooseMode (modes.val.map liftMode) ⦄ := by
  unfold permissions.choose_session_mode permissions.choose_session_mode_loop
  apply loop.spec_decr_nat (fun index => modes.length - index.val)
    (fun index => index.val ≤ modes.length ∧
      chooseModeFrom index.val ((modes.val.map liftMode).drop index.val) =
        chooseMode (modes.val.map liftMode))
  · rintro index ⟨bound, invariant⟩
    simp only [permissions.choose_session_mode_loop.body]
    split
    · rename_i inRange
      have inList : index.val < modes.length := by scalar_tac
      step as ⟨kind, kindIsAt⟩
      have dropped := drop_cons_getElem (modes.val.map liftMode) index.val
        (by simpa using inList)
      rw [dropped] at invariant
      simp only [List.getElem_map] at invariant
      rw [← kindIsAt] at invariant
      cases kind with
      | Bypass =>
        dsimp only
        simp only [WP.spec_ok]
        simpa [liftMode, chooseModeFrom] using invariant
      | Other =>
        dsimp only
        step as ⟨next, nextIs⟩
        refine ⟨by scalar_tac, ?_, by scalar_tac⟩
        rw [nextIs, ← invariant]
        simp [liftMode, chooseModeFrom]
    · rename_i outOfRange
      have atEnd : modes.length ≤ index.val := by scalar_tac
      simp only [WP.spec_ok]
      rw [← invariant, List.drop_eq_nil_of_le (by simpa using atEnd)]
      rfl
  · simp [chooseMode]

/-! ## Queued steering -/

theorem new_refines (settings : Settings) :
    steering.Steering.new Message settings ⦃ runner =>
      liftSteering runner = (Steering.new settings : Steering Message Settings) ⦄ := by
  simp [steering.Steering.new, liftSteering, Steering.new]

/-- `Steering::step` refines the design's `step`, for every runner whose queue Rust can
still push to. -/
theorem step_refines (runner : steering.Steering Message Settings)
    (input : steering.SteeringInput Message Settings)
    (room : runner.queue.length < Usize.max) :
    steering.Steering.step runner input ⦃ effect next =>
      liftEffect effect = (step (liftSteering runner) (liftInput input)).2 ∧
        liftSteering next = (step (liftSteering runner) (liftInput input)).1 ⦄ := by
  cases input with
  | Send message =>
    simp only [steering.Steering.step, steering.Steering.send]
    cases busy : runner.busy
    · simp only [Bool.false_eq_true, ↓reduceIte]
      simp [alloc.vec.FromVecArray.from, liftEffect, liftSteering, liftInput, step, busy]
    · simp only [↓reduceIte]
      step as ⟨queue, queueIs⟩
      simp [liftEffect, liftSteering, liftInput, step, busy, queueIs]
  | TurnEnded =>
    simp only [steering.Steering.step, steering.Steering.end_turn]
    step as ⟨isEmpty, isEmptyIs⟩
    cases queue : runner.queue.val with
    | nil =>
      have empty : isEmpty = true := by simpa [queue] using isEmptyIs
      simp [empty, liftEffect, liftSteering, liftInput, step, queue]
    | cons first rest =>
      have notEmpty : isEmpty = false := by simpa [queue] using isEmptyIs
      simp [notEmpty, liftEffect, liftSteering, liftInput, step, queue]
  | Interrupt =>
    simp only [steering.Steering.step, steering.Steering.interrupt]
    cases busy : runner.busy <;> simp [liftEffect, liftInput, step, liftSteering, busy]
  | ChangeSettings settings =>
    simp [steering.Steering.step, liftEffect, liftSteering, liftInput, step]

/-- Rule 1 holds of the Rust code: `Steering::step` sends `session/cancel` only on `Esc`. -/
theorem rust_cancels_only_on_escape (runner : steering.Steering Message Settings)
    (input : steering.SteeringInput Message Settings)
    (room : runner.queue.length < Usize.max) :
    steering.Steering.step runner input ⦃ effect _ =>
      effect = .Cancel → input = .Interrupt ⦄ := by
  refine WP.spec_mono (step_refines runner input room) ?_
  rintro ⟨effect, next⟩ ⟨effectIs, _⟩ isCancel
  subst isCancel
  have designCancels := cancel_only_on_escape (liftSteering runner) (liftInput input)
    (by simpa [liftEffect] using effectIs.symm)
  cases input <;> simp_all [liftInput]

/-- The ACP client's loop: feed each input to `Steering::step` in turn. -/
@[expose] def runGenerated (runner : steering.Steering Message Settings) :
    List (steering.SteeringInput Message Settings) →
      Result (List (steering.SteeringEffect Message) × steering.Steering Message Settings)
  | [] => ok ([], runner)
  | input :: rest => do
      let (effect, next) ← steering.Steering.step runner input
      let (effects, final) ← runGenerated next rest
      ok (effect :: effects, final)

private theorem step_queue_growth (runner : Steering Message Settings)
    (input : Input Message Settings) :
    (step runner input).1.queue.length ≤ runner.queue.length + 1 := by
  cases input with
  | send message => cases busy : runner.busy <;> simp [step, busy]
  | turnEnded => cases queue : runner.queue <;> simp [step, queue]
  | interrupt => cases busy : runner.busy <;> simp [step, busy]
  | changeSettings settings => simp [step]

/-- The Rust runner, driven through any sequence of inputs, emits exactly the design's
effects and ends in the design's state. So `delivery`, `cost_bound`, `runner_in_class` and
`optimal` hold of the Rust code. The queue must have room for every message in the
sequence, as in `step_refines`. -/
theorem run_refines (runner : steering.Steering Message Settings)
    (inputs : List (steering.SteeringInput Message Settings))
    (room : runner.queue.length + inputs.length < Usize.max) :
    runGenerated runner inputs ⦃ effects final =>
      effects.map liftEffect = (run (liftSteering runner) (inputs.map liftInput)).2 ∧
        liftSteering final = (run (liftSteering runner) (inputs.map liftInput)).1 ⦄ := by
  induction inputs generalizing runner with
  | nil => simp [runGenerated, run]
  | cons input rest inductionHypothesis =>
    unfold runGenerated
    have lengthIsVal : runner.queue.length = (liftSteering runner).queue.length := rfl
    simp only [List.length_cons] at room
    have stepSpec := step_refines runner input (by omega)
    step as ⟨effect, next, effectIs, nextIs⟩
    have grows := step_queue_growth (liftSteering runner) (liftInput input)
    have nextLength : next.queue.length = (liftSteering next).queue.length := rfl
    rw [nextIs] at nextLength
    have restSpec := inductionHypothesis next (by omega)
    step as ⟨effects, final, effectsAre, finalIs⟩
    simp [run, effectIs, effectsAre, finalIs, nextIs]

#print axioms effective_speed_refines
#print axioms settings_effective_speed_refines
#print axioms with_model_refines
#print axioms choose_permission_refines
#print axioms choose_session_mode_refines
#print axioms new_refines
#print axioms step_refines
#print axioms rust_cancels_only_on_escape
#print axioms run_refines

end Velocity.AgentRunner
