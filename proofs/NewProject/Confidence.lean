module

public import NewProject.Design
public import NewProject.Generated.Funs

@[expose] public section

/-!
# New project: confidence

SPEC.md F2. Code-level refinement of the Charon/Aeneas translation of
`velocity_core::new_project::plan_new_project` (`crates/velocity-core/src/new_project.rs`).

The Rust planner returns step tags and decides from an `Observation`: four facts that
the I/O edge reads from the world before acting. `observe` states exactly which fact
each field must hold. `instantiate` is how the I/O edge fills a tag's arguments from the
dialog's request. Under those two, the translated code returns the design's plan step
for step, or the same refusal (`refines`), so the design's `correct`, `cost_bound` and
`optimal` hold for the code (`code_correct`, `code_cost_bound`, `code_optimal`).

Outside these theorems: the file system, `git`, `gh` and the project store that carry
out the steps and supply the observation. Their behaviour is the design's `apply`.
-/

namespace Velocity.NewProject.Confidence

open Aeneas.Std
open Velocity.NewProject.Generated

/-- The tag the Rust planner uses for a design step. -/
def tagOf : Step → new_project.SetupStep
  | .gitInit _ => .GitInit
  | .createRemote _ _ => .CreateRemote
  | .clone _ _ => .Clone
  | .register _ _ => .Register
  | .switchTo _ => .SwitchTo
  | .startInterview _ _ => .StartInterview

def encodeLocation : Location → new_project.NewProjectLocation
  | .localFolder _ initIfNeeded => .LocalFolder initIfNeeded
  | .githubNew _ _ _ => .GitHubNew
  | .githubExisting _ _ => .GitHubExisting

def encodeRefusal : Velocity.NewProject.Error → new_project.Refusal
  | .notARepository => .NotARepository
  | .alreadyAProject => .AlreadyAProject
  | .remoteAlreadyExists => .RemoteAlreadyExists
  | .remoteMissing => .RemoteMissing
  | .targetOccupied => .TargetOccupied

def repositoryOf : Location → Repository
  | .localFolder _ _ => ⟨"", ""⟩
  | .githubNew repository _ _ => repository
  | .githubExisting repository _ => repository

def visibilityOf : Location → Visibility
  | .githubNew _ visibility _ => visibility
  | _ => .privateRepo

/-- What the I/O edge must observe before calling the planner. -/
def observe (request : Request) (world : World) : new_project.Observation where
  already_a_project := decide (request.location.path ∈ registered world)
  is_repository := decide (request.location.path ∈ world.repositories)
  remote_exists := decide ((repositoryOf request.location) ∈ remoteNames world)
  target_occupied := decide (request.location.path ∈ world.occupied)

/-- How the I/O edge fills a tag's arguments from the request. -/
def instantiate (request : Request) : new_project.SetupStep → Step
  | .GitInit => .gitInit request.location.path
  | .CreateRemote => .createRemote (repositoryOf request.location) (visibilityOf request.location)
  | .Clone => .clone (repositoryOf request.location) request.location.path
  | .Register => .register request.location.path request.name
  | .SwitchTo => .switchTo request.location.path
  | .StartInterview => .startInterview request.location.path request.config

/-- The code's result agrees with a design plan: the same steps in the same order, or
the same refusal. -/
def Agrees (code : Result (core.result.Result (alloc.vec.Vec new_project.SetupStep)
    new_project.Refusal)) : Except Velocity.NewProject.Error (List Step) → Prop
  | .ok steps => ∃ tags, code = .ok (.Ok tags) ∧ tags.val = steps.map tagOf
  | .error failure => code = .ok (.Err (encodeRefusal failure))

private theorem push_small {α : Type} (vector : alloc.vec.Vec α) (item : α)
    (small : vector.val.length ≤ 4) :
    ∃ pushed, vector.push item = .ok pushed ∧ pushed.val = vector.val ++ [item] := by
  have fits : vector.val.length + 1 ≤ U32.max := by scalar_tac
  unfold alloc.vec.Vec.push
  simp only [fits, decide_true, Bool.true_or, ↓reduceDIte]
  exact ⟨_, rfl, by simp⟩

private theorem setup_agrees (request : Request) (world : World) :
    Agrees (new_project.setup_steps (encodeLocation request.location)
      (observe request world)) (setupSteps request.location world) := by
  cases hLocation : request.location with
  | localFolder path initIfNeeded =>
    by_cases isRepository : path ∈ world.repositories
    · simp [new_project.setup_steps, encodeLocation, observe, hLocation, Location.path,
        setupSteps, isRepository, Agrees]
    · cases initIfNeeded
      · simp [new_project.setup_steps, encodeLocation, observe, hLocation, Location.path,
          setupSteps, isRepository, Agrees, encodeRefusal]
      · simp [new_project.setup_steps, encodeLocation, observe, hLocation, Location.path,
          setupSteps, isRepository, Agrees, alloc.vec.FromVecArray.from, tagOf]
  | githubNew repository visibility target =>
    by_cases remoteExists : repository ∈ remoteNames world
    · simp [new_project.setup_steps, encodeLocation, observe, hLocation, Location.path,
        repositoryOf, setupSteps, remoteExists, Agrees, encodeRefusal]
    · by_cases targetOccupied : target ∈ world.occupied
      · simp [new_project.setup_steps, encodeLocation, observe, hLocation, Location.path,
          repositoryOf, setupSteps, remoteExists, targetOccupied, Agrees,
          encodeRefusal]
      · simp [new_project.setup_steps, encodeLocation, observe, hLocation, Location.path,
          repositoryOf, setupSteps, remoteExists, targetOccupied, Agrees,
          alloc.vec.FromVecArray.from, tagOf]
  | githubExisting repository target =>
    by_cases remoteExists : repository ∈ remoteNames world
    · by_cases targetOccupied : target ∈ world.occupied
      · simp [new_project.setup_steps, encodeLocation, observe, hLocation, Location.path,
          repositoryOf, setupSteps, remoteExists, targetOccupied, Agrees,
          encodeRefusal]
      · simp [new_project.setup_steps, encodeLocation, observe, hLocation, Location.path,
          repositoryOf, setupSteps, remoteExists, targetOccupied, Agrees,
          alloc.vec.FromVecArray.from, tagOf]
    · simp [new_project.setup_steps, encodeLocation, observe, hLocation, Location.path,
        repositoryOf, setupSteps, remoteExists, Agrees, encodeRefusal]

private theorem setup_short (location : Location) (world : World) (steps : List Step)
    (planned : setupSteps location world = .ok steps) : steps.length ≤ 2 := by
  cases location <;> simp only [setupSteps] at planned <;>
    (repeat' split at planned) <;> cases planned <;> simp

/-- The translated planner returns the design's plan, step for step, or the design's
refusal. -/
theorem refines (request : Request) (world : World) :
    Agrees (new_project.plan_new_project (encodeLocation request.location)
      (observe request world)) (plan request world) := by
  by_cases alreadyRegistered : request.location.path ∈ registered world
  · simp [new_project.plan_new_project, observe, plan, alreadyRegistered, Agrees,
      encodeRefusal]
  · have notFlagged : (observe request world).already_a_project = false := by
      simp [observe, alreadyRegistered]
    have setup := setup_agrees request world
    simp only [new_project.plan_new_project, notFlagged, Bool.false_eq_true, ↓reduceIte,
      plan, alreadyRegistered]
    cases hSetup : setupSteps request.location world with
    | error failure =>
      simp only [hSetup, Agrees] at setup
      simp [setup, Except.map, Agrees]
    | ok steps =>
      simp only [hSetup, Agrees] at setup
      obtain ⟨tags, codeSetup, tagsMatch⟩ := setup
      have short := setup_short request.location world steps hSetup
      have tagsShort : tags.val.length ≤ 2 := by simpa [tagsMatch] using short
      obtain ⟨first, firstPush, firstVal⟩ :=
        push_small tags new_project.SetupStep.Register (by omega)
      obtain ⟨second, secondPush, secondVal⟩ :=
        push_small first new_project.SetupStep.SwitchTo (by simp [firstVal]; omega)
      obtain ⟨third, thirdPush, thirdVal⟩ :=
        push_small second new_project.SetupStep.StartInterview
          (by simp [secondVal, firstVal]; omega)
      refine ⟨third, ?_, ?_⟩
      · simp [codeSetup, firstPush, secondPush, thirdPush]
      · simp [thirdVal, secondVal, firstVal, tagsMatch, finishSteps, tagOf]

/-- Filling the tags from the request gives back the design's steps. -/
theorem instantiate_tags (request : Request) (world : World) (steps : List Step)
    (planned : plan request world = .ok steps) :
    (steps.map tagOf).map (instantiate request) = steps := by
  simp only [plan] at planned
  split at planned
  · cases planned
  · cases hSetup : setupSteps request.location world with
    | error failure => simp [hSetup, Except.map] at planned
    | ok setup =>
      simp only [hSetup, Except.map, Except.ok.injEq] at planned
      subst planned
      cases hLocation : request.location <;>
        simp only [hLocation, setupSteps] at hSetup <;>
        (repeat' split at hSetup) <;> cases hSetup <;>
        simp [finishSteps, tagOf, instantiate, hLocation, Location.path,
          repositoryOf, visibilityOf]

/-- SPEC.md F2 for the code: when the translated planner returns steps, running them as
the I/O edge fills them in sets the project up, registers it, switches to it and starts
its interview. When it refuses, the design refuses too, so nothing runs. -/
theorem code_correct (request : Request) (world : World) :
    (∀ tags, new_project.plan_new_project (encodeLocation request.location)
        (observe request world) = .ok (.Ok tags) →
      ∃ final, execute (tags.val.map (instantiate request)) world = some final ∧
        Meets request final) ∧
    (∀ refusal, new_project.plan_new_project (encodeLocation request.location)
        (observe request world) = .ok (.Err refusal) →
      ∃ failure, plan request world = .error failure ∧ encodeRefusal failure = refusal) := by
  have agrees := refines request world
  constructor
  · intro tags codeOk
    cases planned : plan request world with
    | error failure =>
      simp only [planned, Agrees] at agrees
      rw [agrees] at codeOk
      simp at codeOk
    | ok steps =>
      simp only [planned, Agrees] at agrees
      obtain ⟨codeTags, codeEq, tagsMatch⟩ := agrees
      rw [codeEq] at codeOk
      simp only [Result.ok.injEq, core.result.Result.Ok.injEq] at codeOk
      subst codeOk
      rw [tagsMatch, instantiate_tags request world steps planned]
      exact correct request world steps planned
  · intro refusal codeErr
    cases planned : plan request world with
    | error failure =>
      simp only [planned, Agrees] at agrees
      rw [agrees] at codeErr
      simp only [Result.ok.injEq, core.result.Result.Err.injEq] at codeErr
      exact ⟨failure, rfl, codeErr⟩
    | ok steps =>
      simp only [planned, Agrees] at agrees
      obtain ⟨codeTags, codeEq, _⟩ := agrees
      rw [codeEq] at codeErr
      simp at codeErr

/-- Cost model as in the design (one unit per step): the code plans at most five. -/
theorem code_cost_bound (request : Request) (world : World)
    (tags : alloc.vec.Vec new_project.SetupStep)
    (codeOk : new_project.plan_new_project (encodeLocation request.location)
      (observe request world) = .ok (.Ok tags)) :
    tags.val.length ≤ 5 := by
  have agrees := refines request world
  cases planned : plan request world with
  | error failure =>
    simp only [planned, Agrees] at agrees
    rw [agrees] at codeOk
    simp at codeOk
  | ok steps =>
    simp only [planned, Agrees] at agrees
    obtain ⟨codeTags, codeEq, tagsMatch⟩ := agrees
    rw [codeEq] at codeOk
    simp only [Result.ok.injEq, core.result.Result.Ok.injEq] at codeOk
    subst codeOk
    simpa [tagsMatch] using cost_bound request world steps planned

/-- Optimality for the code, with the design's cost model and class of alternatives:
every finite step sequence that runs from the same well-formed world with no refused
step and meets the request's goal is at least as long as the code's plan. -/
theorem code_optimal (request : Request) (world : World)
    (tags : alloc.vec.Vec new_project.SetupStep) (wellFormed : WellFormed world)
    (codeOk : new_project.plan_new_project (encodeLocation request.location)
      (observe request world) = .ok (.Ok tags))
    (alternative : List Step) (final : World)
    (ran : execute alternative world = some final) (met : Meets request final) :
    tags.val.length ≤ alternative.length := by
  have agrees := refines request world
  cases planned : plan request world with
  | error failure =>
    simp only [planned, Agrees] at agrees
    rw [agrees] at codeOk
    simp at codeOk
  | ok steps =>
    simp only [planned, Agrees] at agrees
    obtain ⟨codeTags, codeEq, tagsMatch⟩ := agrees
    rw [codeEq] at codeOk
    simp only [Result.ok.injEq, core.result.Result.Ok.injEq] at codeOk
    subst codeOk
    simpa [tagsMatch] using
      optimal request world steps wellFormed planned alternative final ran met

#print axioms refines
#print axioms instantiate_tags
#print axioms code_correct
#print axioms code_cost_bound
#print axioms code_optimal

end Velocity.NewProject.Confidence
