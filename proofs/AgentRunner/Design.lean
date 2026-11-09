module
public import Std
public section

/-!
# Agent runner: design

Implements SPEC.md §7 M3 "Agent runner: providers, models, effort, speed (Regular / Fast
where the agent offers it), full permissions, queued steering", which draws on:
- §1 Rule 1: "Every agent runs with full permissions … Velocity has no approval prompts …
  Messages sent to a busy agent are queued and picked up at its next step. They never
  interrupt it."
- §3.2 Agent protocol: "Velocity answers every `session/request_permission` at once with the
  agent's allow-always option. Where an agent offers a bypass mode, `session/set_mode`
  selects it when the session starts." and "A steering message sent mid-turn is queued and
  sent as the next prompt when the turn ends." and "`Esc` interrupt: `session/cancel`".
- F4 Threads and composer: "A message sent while the agent is busy shows 'queued · sends
  when the current step finishes'." and "On a running thread, a new model or speed applies
  from the next message."
- F15 Providers, Speed: "A thread set to Fast that switches to such a model [one without a
  fast mode] runs at Regular, and goes back to Fast if it switches back."

## The model

The runner (`Steering`) receives `Input`s: a message you send, the agent's turn ending,
`Esc`, and a change of model, effort or speed. For each input it updates its state and
emits one `Effect`. A `prompt` effect is sent as one `session/prompt` whose content holds
the messages in order, with the runner's settings after the step (`dispatchSettings`).

Every message queued during a turn goes out together in the prompt sent when that turn
ends. Sending them one per turn instead would hold the second queued message back for a
whole extra turn; `optimal` shows that no runner allowed by Rule 1 and ACP delivers any
message sooner than this one does.

The agent's side is the ACP session (`Session`): it runs at most one turn at a time, so it
accepts `session/prompt` only when no turn is running. `session/cancel` asks the agent to
stop, but the turn still ends with a `turnEnded` signal (stop reason `cancelled`).

## Cost model

`cost_bound` counts `session/prompt` calls, the agent turns the runner starts. Starting from
a new runner, it starts at most one more turn than the agent has ended, and never more turns
than you sent messages. Permission requests cost no waiting at all: `choosePermission` is a
total function of the request, with no state in which an agent waits for you.

`optimal` measures each message's latency as the number of inputs the runner handles after
the message is sent before a `session/prompt` carries it to the agent.

## Class of alternatives

`optimal` quantifies over every runner, deterministic or not, whose actions (`Action`)
on the same inputs:
- are accepted by the ACP session (no prompt while a turn runs), and
- send `session/cancel` only on `Esc` (Rule 1: messages never interrupt the agent).

The second condition is what makes the agent's turn boundaries independent of the runner
until the message is delivered: while a turn runs and nobody cancels it, nothing the runner
does changes when that turn ends, so comparing runners on the same input sequence is fair.
-/

namespace Velocity.AgentRunner

/-! ## Run settings (F15) -/

inductive Provider where
  | anthropic | codex | antigravity | kimiCode
  deriving DecidableEq, Repr

inductive Effort where
  | low | medium | high | max
  deriving DecidableEq, Repr

inductive Speed where
  | regular | fast
  deriving DecidableEq, Repr

/-- `speed` is the speed you chose. It is kept when the model changes, so a thread set to
Fast runs at Fast again when it switches back to a model with a fast mode. -/
structure RunSettings (Model : Type) where
  provider : Provider
  model : Model
  effort : Effort
  speed : Speed

/-- The speed the agent actually runs at. Whether the model has a fast mode is read from
the agent's ACP config options (§3.2), never hard-coded. -/
@[expose] def effectiveSpeed (requested : Speed) (modelHasFastMode : Bool) : Speed :=
  if modelHasFastMode then requested else .regular

@[expose] def RunSettings.withModel {Model : Type} (settings : RunSettings Model)
    (model : Model) : RunSettings Model :=
  { settings with model := model }

/-! ## Full permissions (Rule 1, §3.2) -/

/-- ACP's `PermissionOptionKind`. -/
inductive PermissionKind where
  | allowOnce | allowAlways | rejectOnce | rejectAlways
  deriving DecidableEq, Repr

/-- Scans the options once. `fallback` is the first allow-once option seen so far. -/
@[expose] def choosePermissionFrom (index : Nat) (fallback : Option Nat) :
    List PermissionKind → Option Nat
  | [] => fallback
  | .allowAlways :: _ => some index
  | .allowOnce :: rest =>
      choosePermissionFrom (index + 1) (if fallback.isSome then fallback else some index) rest
  | .rejectOnce :: rest => choosePermissionFrom (index + 1) fallback rest
  | .rejectAlways :: rest => choosePermissionFrom (index + 1) fallback rest

/-- The option Velocity answers a permission request with: the first allow-always option,
or the first allow-once option when the agent offers no allow-always option. -/
@[expose] def choosePermission (options : List PermissionKind) : Option Nat :=
  choosePermissionFrom 0 none options

/-- Session modes, classified by the provider layer: `bypass` is a mode that skips the
agent's own permission checks (§3.2). -/
inductive SessionModeKind where
  | bypass | other
  deriving DecidableEq, Repr

@[expose] def chooseModeFrom (index : Nat) : List SessionModeKind → Option Nat
  | [] => none
  | .bypass :: _ => some index
  | .other :: rest => chooseModeFrom (index + 1) rest

/-- The mode selected with `session/set_mode` when the session starts. -/
@[expose] def chooseMode (modes : List SessionModeKind) : Option Nat := chooseModeFrom 0 modes

/-! ## Queued steering (Rule 1, F4) -/

structure Steering (Message Settings : Type) where
  busy : Bool
  queue : List Message
  settings : Settings

inductive Input (Message Settings : Type) where
  | send (message : Message)
  | turnEnded
  | interrupt
  | changeSettings (settings : Settings)

inductive Effect (Message : Type) where
  /-- One `session/prompt` holding these messages, in order. -/
  | prompt (messages : List Message)
  /-- The message waits for the turn to end ("queued · sends when the current step
  finishes"). -/
  | queued
  /-- `session/cancel`. -/
  | cancel
  | nothing

variable {Message Settings : Type}

@[expose] def Steering.new (settings : Settings) : Steering Message Settings :=
  { busy := false, queue := [], settings := settings }

@[expose] def step (runner : Steering Message Settings) :
    Input Message Settings → Steering Message Settings × Effect Message
  | .send message =>
      if runner.busy then ({ runner with queue := runner.queue ++ [message] }, .queued)
      else ({ runner with busy := true }, .prompt [message])
  | .turnEnded =>
      match runner.queue with
      | [] => ({ runner with busy := false }, .nothing)
      | queued => ({ runner with busy := true, queue := [] }, .prompt queued)
  | .interrupt => if runner.busy then (runner, .cancel) else (runner, .nothing)
  | .changeSettings settings => ({ runner with settings := settings }, .nothing)

/-- The settings a prompt effect is sent with: the runner's settings after the step. -/
@[expose] def dispatchSettings (runner : Steering Message Settings)
    (input : Input Message Settings) : Settings :=
  (step runner input).1.settings

@[expose] def run (runner : Steering Message Settings) :
    List (Input Message Settings) → Steering Message Settings × List (Effect Message)
  | [] => (runner, [])
  | input :: rest =>
      let (next, effect) := step runner input
      let (final, effects) := run next rest
      (final, effect :: effects)

@[expose] def promptedMessages : List (Effect Message) → List Message
  | [] => []
  | .prompt messages :: rest => messages ++ promptedMessages rest
  | _ :: rest => promptedMessages rest

@[expose] def promptCount : List (Effect Message) → Nat
  | [] => 0
  | .prompt _ :: rest => promptCount rest + 1
  | _ :: rest => promptCount rest

@[expose] def sentMessages : List (Input Message Settings) → List Message
  | [] => []
  | .send message :: rest => message :: sentMessages rest
  | _ :: rest => sentMessages rest

@[expose] def turnEndCount : List (Input Message Settings) → Nat
  | [] => 0
  | .turnEnded :: rest => turnEndCount rest + 1
  | _ :: rest => turnEndCount rest

/-- An idle runner has nothing queued. -/
@[expose] def WellFormed (runner : Steering Message Settings) : Prop :=
  runner.busy = false → runner.queue = []

/-- The agent ends a turn only while one runs. -/
@[expose] def ValidInputs (runner : Steering Message Settings) :
    List (Input Message Settings) → Prop
  | [] => True
  | input :: rest =>
      (input = .turnEnded → runner.busy = true) ∧ ValidInputs (step runner input).1 rest

/-! ## The ACP session and the class of alternative runners -/

inductive Action (Message : Type) where
  | prompt (messages : List Message)
  | cancel

/-- What an input does to the session before the runner acts on it. The session's state is
whether a turn is running. An idle session never ends a turn. -/
@[expose] def signal (turnRunning : Bool) : Input Message Settings → Option Bool
  | .turnEnded => if turnRunning then some false else none
  | _ => some turnRunning

/-- ACP runs one turn at a time. A cancelled turn keeps running until the agent ends it. -/
@[expose] def perform (turnRunning : Bool) : Action Message → Option Bool
  | .prompt _ => if turnRunning then none else some true
  | .cancel => some turnRunning

@[expose] def performAll (turnRunning : Bool) : List (Action Message) → Option Bool
  | [] => some turnRunning
  | action :: rest =>
      match perform turnRunning action with
      | none => none
      | some next => performAll next rest

@[expose] def CancelsOnlyOnEscape (input : Input Message Settings)
    (actions : List (Action Message)) : Prop :=
  Action.cancel ∈ actions → input = .interrupt

/-- A run of any runner in the class: each input paired with the actions taken on it. -/
@[expose] def InClass (turnRunning : Bool) :
    List (Input Message Settings × List (Action Message)) → Prop
  | [] => True
  | (input, actions) :: rest =>
      ∃ afterSignal afterActions, signal turnRunning input = some afterSignal ∧
        performAll afterSignal actions = some afterActions ∧
        CancelsOnlyOnEscape input actions ∧ InClass afterActions rest

@[expose] def Effect.actions : Effect Message → List (Action Message)
  | .prompt messages => [.prompt messages]
  | .cancel => [.cancel]
  | .queued => []
  | .nothing => []

@[expose] def Delivers (message : Message) (actions : List (Action Message)) : Prop :=
  ∃ messages, Action.prompt messages ∈ actions ∧ message ∈ messages

/-! ## Proofs: run settings and permissions -/

theorem speed_follows_model (requested : Speed) (hasFastMode : Bool) :
    effectiveSpeed requested hasFastMode = .fast ↔
      requested = .fast ∧ hasFastMode = true := by
  cases requested <;> cases hasFastMode <;> simp [effectiveSpeed]

/-- F15: switching to a model without a fast mode runs at Regular; switching back restores
the speed you chose, because the chosen speed is never overwritten. -/
theorem speed_round_trip {Model : Type} (settings : RunSettings Model) (other : Model) :
    effectiveSpeed (settings.withModel other).speed false = .regular ∧
      (settings.withModel other).withModel settings.model = settings ∧
      effectiveSpeed ((settings.withModel other).withModel settings.model).speed true =
        settings.speed := by
  refine ⟨rfl, rfl, rfl⟩

private theorem choosePermissionFrom_allows (index : Nat) (fallback : Option Nat)
    (options : List PermissionKind) (chosen : Nat)
    (result : choosePermissionFrom index fallback options = some chosen) :
    (fallback = some chosen) ∨
      (index ≤ chosen ∧ (options[chosen - index]? = some .allowAlways ∨
        options[chosen - index]? = some .allowOnce)) := by
  induction options generalizing index fallback with
  | nil => exact Or.inl result
  | cons kind rest inductionHypothesis =>
    cases kind with
    | allowAlways =>
      simp only [choosePermissionFrom, Option.some.injEq] at result
      subst result
      right
      simp
    | allowOnce =>
      simp only [choosePermissionFrom] at result
      by_cases someFallback : fallback.isSome
      · simp only [someFallback, ↓reduceIte] at result
        rcases inductionHypothesis (index + 1) fallback result with found | ⟨bound, found⟩
        · exact Or.inl found
        · right
          refine ⟨by omega, ?_⟩
          have shift : chosen - index = (chosen - (index + 1)) + 1 := by omega
          rw [shift]
          simpa using found
      · simp only [someFallback, Bool.false_eq_true, ↓reduceIte] at result
        rcases inductionHypothesis (index + 1) (some index) result with found | ⟨bound, found⟩
        · simp only [Option.some.injEq] at found
          subst found
          right
          simp
        · right
          refine ⟨by omega, ?_⟩
          have shift : chosen - index = (chosen - (index + 1)) + 1 := by omega
          rw [shift]
          simpa using found
    | rejectOnce | rejectAlways =>
      simp only [choosePermissionFrom] at result
      rcases inductionHypothesis (index + 1) fallback result with found | ⟨bound, found⟩
      · exact Or.inl found
      · right
        refine ⟨by omega, ?_⟩
        have shift : chosen - index = (chosen - (index + 1)) + 1 := by omega
        rw [shift]
        simpa using found

/-- Rule 1: Velocity never answers a permission request with a rejection. -/
theorem choosePermission_allows (options : List PermissionKind) (chosen : Nat)
    (result : choosePermission options = some chosen) :
    options[chosen]? = some .allowAlways ∨ options[chosen]? = some .allowOnce := by
  rcases choosePermissionFrom_allows 0 none options chosen result with impossible | ⟨_, found⟩
  · cases impossible
  · simpa using found

private theorem choosePermissionFrom_prefers_always (index : Nat) (fallback : Option Nat)
    (options : List PermissionKind) (hasAlways : PermissionKind.allowAlways ∈ options) :
    ∃ chosen, choosePermissionFrom index fallback options = some chosen ∧
      index ≤ chosen ∧ options[chosen - index]? = some .allowAlways := by
  induction options generalizing index fallback with
  | nil => cases hasAlways
  | cons kind rest inductionHypothesis =>
    by_cases isAlways : kind = .allowAlways
    · subst isAlways
      exact ⟨index, by simp [choosePermissionFrom]⟩
    · have inRest : PermissionKind.allowAlways ∈ rest := by
        rcases List.mem_cons.mp hasAlways with first | later
        · exact absurd first.symm isAlways
        · exact later
      have unfolds : ∃ nextFallback, choosePermissionFrom index fallback (kind :: rest) =
          choosePermissionFrom (index + 1) nextFallback rest := by
        cases kind with
        | allowAlways => exact absurd rfl isAlways
        | allowOnce => exact ⟨_, rfl⟩
        | rejectOnce => exact ⟨_, rfl⟩
        | rejectAlways => exact ⟨_, rfl⟩
      obtain ⟨nextFallback, unfolded⟩ := unfolds
      obtain ⟨chosen, result, bound, found⟩ := inductionHypothesis (index + 1) nextFallback inRest
      refine ⟨chosen, unfolded ▸ result, by omega, ?_⟩
      have shift : chosen - index = (chosen - (index + 1)) + 1 := by omega
      rw [shift]
      simpa using found

/-- §3.2: when the agent offers an allow-always option, Velocity picks one. -/
theorem choosePermission_prefers_always (options : List PermissionKind)
    (hasAlways : PermissionKind.allowAlways ∈ options) :
    ∃ chosen, choosePermission options = some chosen ∧
      options[chosen]? = some .allowAlways := by
  obtain ⟨chosen, result, _, found⟩ := choosePermissionFrom_prefers_always 0 none options hasAlways
  exact ⟨chosen, result, by simpa using found⟩

private theorem choosePermissionFrom_answers (index : Nat) (fallback : Option Nat)
    (options : List PermissionKind)
    (hasAllow : PermissionKind.allowAlways ∈ options ∨ PermissionKind.allowOnce ∈ options ∨
      fallback.isSome) :
    (choosePermissionFrom index fallback options).isSome := by
  induction options generalizing index fallback with
  | nil => simpa [choosePermissionFrom] using hasAllow
  | cons kind rest inductionHypothesis =>
    cases kind with
    | allowAlways => simp [choosePermissionFrom]
    | allowOnce =>
      simp only [choosePermissionFrom]
      apply inductionHypothesis
      right; right
      split <;> simp_all
    | rejectOnce | rejectAlways =>
      simp only [choosePermissionFrom]
      apply inductionHypothesis
      simpa using hasAllow

/-- Every request that offers any allow option is answered at once. -/
theorem choosePermission_answers (options : List PermissionKind)
    (hasAllow : PermissionKind.allowAlways ∈ options ∨ PermissionKind.allowOnce ∈ options) :
    (choosePermission options).isSome :=
  choosePermissionFrom_answers 0 none options (by simpa using hasAllow)

private theorem chooseModeFrom_spec (index : Nat) (modes : List SessionModeKind) :
    chooseModeFrom index modes = (modes.findIdx? (· = .bypass)).map (· + index) := by
  induction modes generalizing index with
  | nil => rfl
  | cons mode rest inductionHypothesis =>
    cases mode with
    | bypass => simp [chooseModeFrom, List.findIdx?_cons]
    | other =>
      simp only [chooseModeFrom, inductionHypothesis, List.findIdx?_cons]
      simp only [reduceCtorEq, decide_false, Bool.false_eq_true, ↓reduceIte, Option.map_map]
      congr 1
      funext position
      simp only [Function.comp_apply]
      omega

/-- §3.2: the first bypass mode the agent offers is selected; none is selected only when
the agent offers none. -/
theorem chooseMode_bypass (modes : List SessionModeKind) :
    chooseMode modes = modes.findIdx? (· = .bypass) := by
  simp [chooseMode, chooseModeFrom_spec]

/-! ## Proofs: queued steering -/

theorem step_wellFormed (runner : Steering Message Settings) (input : Input Message Settings)
    (wellFormed : WellFormed runner) : WellFormed (step runner input).1 := by
  cases input with
  | send message =>
    cases busy : runner.busy <;> simp [step, busy, WellFormed]
  | turnEnded =>
    simp only [step]
    split <;> simp_all [WellFormed]
  | interrupt =>
    simp only [step]
    split <;> exact wellFormed
  | changeSettings settings =>
    simpa [step, WellFormed] using wellFormed

/-- Rule 1: only `Esc` cancels a turn. A message never does. -/
theorem cancel_only_on_escape (runner : Steering Message Settings)
    (input : Input Message Settings) (cancels : (step runner input).2 = .cancel) :
    input = .interrupt := by
  cases input with
  | send message =>
    simp only [step] at cancels
    split at cancels <;> cases cancels
  | turnEnded =>
    simp only [step] at cancels
    split at cancels <;> cases cancels
  | interrupt => rfl
  | changeSettings settings => cases cancels

/-- Rule 1, F4: a message sent while the agent is busy is queued; it does not reach the
agent and does not touch the running turn. -/
theorem busy_send_queues (runner : Steering Message Settings) (message : Message)
    (busy : runner.busy = true) :
    step runner (.send message) =
      ({ runner with queue := runner.queue ++ [message] }, .queued) := by
  simp [step, busy]

/-- A message sent to an idle agent is prompted at once. -/
theorem idle_send_prompts (runner : Steering Message Settings) (message : Message)
    (idle : runner.busy = false) :
    step runner (.send message) = ({ runner with busy := true }, .prompt [message]) := by
  simp [step, idle]

/-- §3.2: when a turn ends, everything queued goes out as the next prompt. -/
theorem turn_end_sends_queue (runner : Steering Message Settings)
    (queued : runner.queue ≠ []) :
    step runner .turnEnded =
      ({ runner with busy := true, queue := [] }, .prompt runner.queue) := by
  cases queue : runner.queue with
  | nil => exact absurd queue queued
  | cons first rest => simp [step, queue]

/-- F4: a new model, effort or speed changes nothing about the running turn and is the one
every later prompt is sent with, until the next change. -/
theorem settings_apply_from_next_message (runner : Steering Message Settings)
    (settings : Settings) (input : Input Message Settings) (messages : List Message)
    (prompts : (step { runner with settings := settings } input).2 = .prompt messages) :
    step runner (.changeSettings settings) = ({ runner with settings := settings }, .nothing) ∧
      dispatchSettings { runner with settings := settings } input = settings := by
  refine ⟨rfl, ?_⟩
  cases input with
  | send message =>
    simp only [dispatchSettings, step]
    split <;> rfl
  | turnEnded =>
    simp only [dispatchSettings, step]
    split <;> rfl
  | interrupt =>
    simp only [step] at prompts
    split at prompts <;> cases prompts
  | changeSettings other => cases prompts

private theorem step_delivery (runner : Steering Message Settings)
    (input : Input Message Settings) (wellFormed : WellFormed runner) :
    promptedMessages [(step runner input).2] ++ (step runner input).1.queue =
      runner.queue ++ sentMessages [input] := by
  cases input with
  | send message =>
    cases busy : runner.busy
    · simp [step, busy, wellFormed busy, promptedMessages, sentMessages]
    · simp [step, busy, promptedMessages, sentMessages]
  | turnEnded =>
    simp only [step]
    split <;> simp_all [promptedMessages, sentMessages]
  | interrupt =>
    simp only [step]
    split <;> simp [promptedMessages, sentMessages]
  | changeSettings settings => simp [step, promptedMessages, sentMessages]

private theorem promptedMessages_cons (effect : Effect Message) (rest : List (Effect Message)) :
    promptedMessages (effect :: rest) = promptedMessages [effect] ++ promptedMessages rest := by
  cases effect <;> simp [promptedMessages]

private theorem sentMessages_cons (input : Input Message Settings)
    (rest : List (Input Message Settings)) :
    sentMessages (input :: rest) = sentMessages [input] ++ sentMessages rest := by
  cases input <;> simp [sentMessages]

/-- Rule 1: every message reaches the agent exactly once, in the order you sent it. What
has not gone out yet is still queued. Nothing is lost or reordered. -/
theorem delivery (runner : Steering Message Settings) (inputs : List (Input Message Settings))
    (wellFormed : WellFormed runner) :
    promptedMessages (run runner inputs).2 ++ (run runner inputs).1.queue =
      runner.queue ++ sentMessages inputs := by
  induction inputs generalizing runner with
  | nil => simp [run, promptedMessages, sentMessages]
  | cons input rest inductionHypothesis =>
    have next := inductionHypothesis (step runner input).1 (step_wellFormed runner input wellFormed)
    have here := step_delivery runner input wellFormed
    simp only [run]
    rw [promptedMessages_cons, sentMessages_cons, List.append_assoc, next, ← List.append_assoc,
      here, List.append_assoc]

/-- The runner's own actions, paired with the inputs they answer. -/
@[expose] def runActions (runner : Steering Message Settings) :
    List (Input Message Settings) → List (Input Message Settings × List (Action Message))
  | [] => []
  | input :: rest => (input, (step runner input).2.actions) :: runActions (step runner input).1 rest

/-- The runner itself is in the class `optimal` ranges over: ACP accepts every action it
takes, and it cancels only on `Esc`. -/
theorem runner_in_class (runner : Steering Message Settings)
    (inputs : List (Input Message Settings)) (valid : ValidInputs runner inputs) :
    InClass runner.busy (runActions runner inputs) := by
  induction inputs generalizing runner with
  | nil => trivial
  | cons input rest inductionHypothesis =>
    obtain ⟨endsOnlyWhenBusy, validRest⟩ := valid
    have restInClass := inductionHypothesis (step runner input).1 validRest
    have escapeOnly : CancelsOnlyOnEscape input (step runner input).2.actions := by
      intro hasCancel
      apply cancel_only_on_escape runner input
      revert hasCancel
      cases (step runner input).2 <;> simp [Effect.actions]
    simp only [runActions, InClass]
    cases input with
    | send message =>
      refine ⟨runner.busy, _, rfl, ?_, escapeOnly, restInClass⟩
      cases busy : runner.busy <;> simp [step, busy, Effect.actions, performAll, perform]
    | turnEnded =>
      have busy := endsOnlyWhenBusy rfl
      refine ⟨false, _, by simp [signal, busy], ?_, escapeOnly, restInClass⟩
      cases queue : runner.queue <;> simp [step, queue, Effect.actions, performAll, perform]
    | interrupt =>
      refine ⟨runner.busy, _, rfl, ?_, escapeOnly, restInClass⟩
      cases busy : runner.busy <;> simp [step, busy, Effect.actions, performAll, perform]
    | changeSettings settings =>
      refine ⟨runner.busy, _, rfl, ?_, escapeOnly, restInClass⟩
      simp [step, Effect.actions, performAll]

/-- The pieces of `correct` the steering part of the runner contributes. -/
@[expose] def SteeringCorrect (Message Settings : Type) : Prop :=
  (∀ (runner : Steering Message Settings) input,
      (step runner input).2 = .cancel → input = .interrupt) ∧
  (∀ (runner : Steering Message Settings) message, runner.busy = true →
      step runner (.send message) =
        ({ runner with queue := runner.queue ++ [message] }, .queued)) ∧
  (∀ (runner : Steering Message Settings) inputs, WellFormed runner →
      promptedMessages (run runner inputs).2 ++ (run runner inputs).1.queue =
        runner.queue ++ sentMessages inputs) ∧
  (∀ (runner : Steering Message Settings) inputs, ValidInputs runner inputs →
      InClass runner.busy (runActions runner inputs)) ∧
  (∀ (runner : Steering Message Settings) (settings : Settings)
      (input : Input Message Settings) (messages : List Message),
      (step { runner with settings := settings } input).2 = .prompt messages →
      dispatchSettings { runner with settings := settings } input = settings)

/-- The runner meets the M3 item: Fast only where the model has it and restored on the
way back (F15), every permission answered with an allow option and allow-always preferred
(Rule 1, §3.2), the bypass mode selected where offered (§3.2), and queued steering that
never interrupts, never loses or reorders a message, stays within ACP, and sends each
prompt with the settings in force when it is sent (Rule 1, F4). -/
theorem correct :
    (∀ requested hasFastMode, effectiveSpeed requested hasFastMode = .fast ↔
      requested = .fast ∧ hasFastMode = true) ∧
    (∀ options chosen, choosePermission options = some chosen →
      options[chosen]? = some .allowAlways ∨ options[chosen]? = some .allowOnce) ∧
    (∀ options, PermissionKind.allowAlways ∈ options →
      ∃ chosen, choosePermission options = some chosen ∧
        options[chosen]? = some .allowAlways) ∧
    (∀ modes, chooseMode modes = modes.findIdx? (· = .bypass)) ∧
    SteeringCorrect Message Settings :=
  ⟨speed_follows_model, choosePermission_allows, choosePermission_prefers_always,
    chooseMode_bypass,
    cancel_only_on_escape, busy_send_queues, delivery, runner_in_class,
    fun runner settings input messages prompts =>
      (settings_apply_from_next_message runner settings input messages prompts).2⟩

/-! ## Cost -/

private theorem busyNat_step (runner : Steering Message Settings)
    (input : Input Message Settings) (endsOnlyWhenBusy : input = .turnEnded → runner.busy = true) :
    promptCount [(step runner input).2] + runner.busy.toNat =
      turnEndCount [input] + (step runner input).1.busy.toNat := by
  cases input with
  | send message =>
    cases busy : runner.busy <;> simp [step, busy, promptCount, turnEndCount]
  | turnEnded =>
    have busy := endsOnlyWhenBusy rfl
    simp only [step]
    split <;> simp [busy, promptCount, turnEndCount]
  | interrupt =>
    simp only [step]
    split <;> simp [promptCount, turnEndCount]
  | changeSettings settings => simp [step, promptCount, turnEndCount]

private theorem promptCount_cons (effect : Effect Message) (rest : List (Effect Message)) :
    promptCount (effect :: rest) = promptCount [effect] + promptCount rest := by
  cases effect <;> simp [promptCount] <;> omega

private theorem turnEndCount_cons (input : Input Message Settings)
    (rest : List (Input Message Settings)) :
    turnEndCount (input :: rest) = turnEndCount [input] + turnEndCount rest := by
  cases input <;> simp [turnEndCount] <;> omega

/-- Every turn the runner starts is either its first or follows a turn the agent ended. -/
theorem prompts_balance (runner : Steering Message Settings)
    (inputs : List (Input Message Settings)) (valid : ValidInputs runner inputs) :
    promptCount (run runner inputs).2 + runner.busy.toNat =
      turnEndCount inputs + (run runner inputs).1.busy.toNat := by
  induction inputs generalizing runner with
  | nil => simp [run, promptCount, turnEndCount]
  | cons input rest inductionHypothesis =>
    obtain ⟨endsOnlyWhenBusy, validRest⟩ := valid
    have next := inductionHypothesis (step runner input).1 validRest
    have here := busyNat_step runner input endsOnlyWhenBusy
    simp only [run]
    rw [promptCount_cons, turnEndCount_cons]
    omega

private theorem promptCount_le_messages (effects : List (Effect Message))
    (nonempty : ∀ messages, Effect.prompt messages ∈ effects → messages ≠ []) :
    promptCount effects ≤ (promptedMessages effects).length := by
  induction effects with
  | nil => simp [promptCount]
  | cons effect rest inductionHypothesis =>
    have restBound := inductionHypothesis (fun messages inRest =>
      nonempty messages (List.mem_cons_of_mem _ inRest))
    cases effect with
    | prompt messages =>
      have notEmpty := nonempty messages List.mem_cons_self
      have positive : 0 < messages.length := List.length_pos_iff.mpr notEmpty
      simp only [promptCount, promptedMessages, List.length_append]
      omega
    | queued | cancel | nothing => simpa [promptCount, promptedMessages] using restBound

private theorem prompts_nonempty (runner : Steering Message Settings)
    (inputs : List (Input Message Settings)) :
    ∀ messages, Effect.prompt messages ∈ (run runner inputs).2 → messages ≠ [] := by
  induction inputs generalizing runner with
  | nil => simp [run]
  | cons input rest inductionHypothesis =>
    intro messages member
    simp only [run, List.mem_cons] at member
    rcases member with here | later
    · cases input with
      | send message =>
        simp only [step] at here
        split at here <;> simp_all
      | turnEnded =>
        simp only [step] at here
        split at here
        · cases here
        · rename_i queued notEmpty
          simp only [Effect.prompt.injEq] at here
          subst here
          exact notEmpty
      | interrupt =>
        simp only [step] at here
        split at here <;> cases here
      | changeSettings settings => cases here
    · exact inductionHypothesis (step runner input).1 messages later

private theorem sentMessages_length_le (inputs : List (Input Message Settings)) :
    (sentMessages inputs).length ≤ inputs.length := by
  induction inputs with
  | nil => simp [sentMessages]
  | cons input rest inductionHypothesis =>
    cases input <;> simp [sentMessages] <;> omega

/-- Cost model: one unit per `session/prompt` call, that is per agent turn the runner
starts. From a new runner, on any inputs ACP can produce, the runner starts at most one more
turn than the agent has ended, and no more turns than you sent messages. -/
theorem cost_bound (settings : Settings) (inputs : List (Input Message Settings))
    (valid : ValidInputs (Steering.new settings : Steering Message Settings) inputs) :
    promptCount (run (Steering.new settings : Steering Message Settings) inputs).2 ≤
        turnEndCount inputs + 1 ∧
      promptCount (run (Steering.new settings : Steering Message Settings) inputs).2 ≤
        (sentMessages inputs).length := by
  have startIdle : (Steering.new settings : Steering Message Settings).busy.toNat = 0 := rfl
  have startEmpty : (Steering.new settings : Steering Message Settings).queue = [] := rfl
  constructor
  · have balance := prompts_balance (Steering.new settings : Steering Message Settings) inputs valid
    have finalBusy := Bool.toNat_le (run (Steering.new settings : Steering Message Settings)
      inputs).1.busy
    omega
  · have delivered := delivery (Steering.new settings : Steering Message Settings) inputs
      (fun _ => rfl)
    have bound := promptCount_le_messages _ (prompts_nonempty
      (Steering.new settings : Steering Message Settings) inputs)
    have lengths := congrArg List.length delivered
    rw [startEmpty, List.nil_append, List.length_append] at lengths
    omega

/-! ## Optimality -/

private theorem performAll_busy_no_prompt (actions : List (Action Message)) (after : Bool)
    (accepted : performAll true actions = some after) :
    after = true ∧ ∀ messages, Action.prompt messages ∉ actions := by
  induction actions with
  | nil =>
    simp only [performAll, Option.some.injEq] at accepted
    exact ⟨accepted.symm, fun _ member => by cases member⟩
  | cons action rest inductionHypothesis =>
    cases action with
    | prompt messages => simp [performAll, perform] at accepted
    | cancel =>
      simp only [performAll, perform] at accepted
      obtain ⟨stillBusy, noPrompt⟩ := inductionHypothesis accepted
      refine ⟨stillBusy, fun messages member => ?_⟩
      simp only [List.mem_cons, reduceCtorEq, false_or] at member
      exact noPrompt messages member

/-- Lower bound for every runner in the class: while a turn runs and the agent has not
ended it, no runner can send a prompt, so it cannot deliver anything. -/
private theorem no_delivery_before_turn_end (message : Message)
    (rounds : List (Input Message Settings × List (Action Message)))
    (inClass : InClass true rounds) (round : Nat)
    (beforeTurnEnd : ∀ earlier, earlier ≤ round →
      (rounds[earlier]?.map Prod.fst) ≠ some .turnEnded)
    (actions : List (Action Message))
    (atRound : (rounds[round]?.map Prod.snd) = some actions) :
    ¬ Delivers message actions := by
  induction rounds generalizing round with
  | nil => simp at atRound
  | cons first rest inductionHypothesis =>
    obtain ⟨input, firstActions⟩ := first
    obtain ⟨afterSignal, afterActions, signalled, performed, _, restInClass⟩ := inClass
    have notEnd : input ≠ .turnEnded := by
      have := beforeTurnEnd 0 (Nat.zero_le _)
      simpa using this
    have stillBusy : afterSignal = true := by
      cases input <;> simp_all [signal]
    subst stillBusy
    obtain ⟨busyAfter, noPrompt⟩ := performAll_busy_no_prompt firstActions afterActions performed
    cases round with
    | zero =>
      simp only [List.getElem?_cons_zero, Option.map_some, Option.some.injEq] at atRound
      subst atRound
      rintro ⟨messages, member, _⟩
      exact noPrompt messages member
    | succ earlierRound =>
      subst busyAfter
      apply inductionHypothesis restInClass earlierRound
      · intro earlier bound
        have := beforeTurnEnd (earlier + 1) (by omega)
        simpa using this
      · simpa using atRound

private theorem queue_keeps_message (runner : Steering Message Settings) (message : Message)
    (rest : List (Input Message Settings)) (busy : runner.busy = true)
    (queued : message ∈ runner.queue)
    (noTurnEnd : ∀ input ∈ rest, input ≠ .turnEnded) :
    (run runner rest).1.busy = true ∧ message ∈ (run runner rest).1.queue ∧
      ∀ effect ∈ (run runner rest).2, ∀ messages, effect ≠ .prompt messages := by
  induction rest generalizing runner with
  | nil => exact ⟨busy, queued, by simp [run]⟩
  | cons input remaining inductionHypothesis =>
    have notEnd := noTurnEnd input List.mem_cons_self
    have remainingNoEnd : ∀ later ∈ remaining, later ≠ .turnEnded :=
      fun later member => noTurnEnd later (List.mem_cons_of_mem _ member)
    have afterStep : (step runner input).1.busy = true ∧ message ∈ (step runner input).1.queue ∧
        ∀ messages, (step runner input).2 ≠ .prompt messages := by
      cases input with
      | send sent => simp [step, busy, queued]
      | turnEnded => exact absurd rfl notEnd
      | interrupt => simp [step, busy, queued]
      | changeSettings settings => simp [step, busy, queued]
    obtain ⟨stillBusy, stillQueued, noPrompt⟩ := afterStep
    obtain ⟨finalBusy, finalQueued, laterNoPrompt⟩ :=
      inductionHypothesis (step runner input).1 stillBusy stillQueued remainingNoEnd
    refine ⟨finalBusy, finalQueued, ?_⟩
    intro effect member messages
    simp only [run, List.mem_cons] at member
    rcases member with here | later
    · subst here
      exact noPrompt messages
    · exact laterNoPrompt effect later messages

/-- Latency: a message sent while a turn runs reaches the agent in the prompt sent at the
very end of that turn (after the `waiting` inputs that are not turn ends), and no runner in
the class (ACP accepts its actions, it cancels only on `Esc`) delivers it any sooner on the
same inputs. A message sent to an idle agent is delivered with no wait at all, which no
runner can beat. Cost: the number of inputs handled between sending the message and the
prompt that carries it. -/
theorem optimal (runner : Steering Message Settings) (message : Message)
    (waiting : List (Input Message Settings)) (later : List (Input Message Settings))
    (noTurnEnd : ∀ input ∈ waiting, input ≠ .turnEnded) :
    (runner.busy = false →
      Delivers message (step runner (.send message)).2.actions) ∧
    (runner.busy = true →
      (Delivers message
          (step (run (step runner (.send message)).1 waiting).1 .turnEnded).2.actions) ∧
      ∀ (alternative : List (Input Message Settings × List (Action Message))),
        alternative.map Prod.fst = .send message :: waiting ++ .turnEnded :: later →
        InClass true alternative →
        ∀ round actions, (alternative[round]?.map Prod.snd) = some actions →
          Delivers message actions → waiting.length + 1 ≤ round) := by
  refine ⟨fun idle => ?_, fun busy => ⟨?_, ?_⟩⟩
  · simp only [step, idle, Bool.false_eq_true, ↓reduceIte, Effect.actions]
    exact ⟨[message], List.mem_singleton_self _, List.mem_singleton_self _⟩
  · have queued := busy_send_queues runner message busy
    obtain ⟨stillBusy, stillQueued, _⟩ := queue_keeps_message (step runner (.send message)).1
      message waiting (by simp [queued, busy]) (by simp [queued]) noTurnEnd
    have notEmpty : (run (step runner (.send message)).1 waiting).1.queue ≠ [] :=
      List.ne_nil_of_mem stillQueued
    rw [turn_end_sends_queue _ notEmpty]
    exact ⟨_, List.mem_singleton_self _, stillQueued⟩
  · intro alternative inputs inClass round actions atRound delivers
    refine Nat.le_of_not_lt fun early => ?_
    apply no_delivery_before_turn_end message alternative inClass round _ actions atRound delivers
    intro earlier bound
    have inputAt := congrArg (·[earlier]?) inputs
    simp only [List.getElem?_map] at inputAt
    rw [inputAt]
    cases earlier with
    | zero => simp
    | succ position =>
      have inWaiting : position < waiting.length := by omega
      simp only [List.cons_append, List.getElem?_cons_succ,
        List.getElem?_append_left inWaiting, List.getElem?_eq_getElem inWaiting,
        ne_eq, Option.some.injEq]
      exact noTurnEnd _ (List.getElem_mem inWaiting)

#print axioms speed_follows_model
#print axioms speed_round_trip
#print axioms choosePermission_allows
#print axioms choosePermission_prefers_always
#print axioms choosePermission_answers
#print axioms chooseMode_bypass
#print axioms step_wellFormed
#print axioms cancel_only_on_escape
#print axioms busy_send_queues
#print axioms idle_send_prompts
#print axioms turn_end_sends_queue
#print axioms settings_apply_from_next_message
#print axioms delivery
#print axioms runner_in_class
#print axioms correct
#print axioms prompts_balance
#print axioms cost_bound
#print axioms optimal

end Velocity.AgentRunner
