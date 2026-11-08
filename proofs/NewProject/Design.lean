module
public import Std
public section

/-!
# New project: design

Implements SPEC.md F2 · New project, acceptance criteria:
- "Create sets up the project (folder / `git init` / repo / clone), switches to it, and
  starts the spec interview thread straight away." (`correct`)
- "Esc cancels; nothing is created." (`cancel_creates_nothing`, `error_creates_nothing`)

The dialog's answers form a `Request`. Velocity observes the file system, GitHub and its
own project store (the `World`), then `plan` computes, purely and before any side effect,
the list of `Step`s that set the project up. A request the world cannot satisfy is
refused with an `Error` before anything runs, so a refusal also creates nothing.

Paths are compared after the Rust side normalises them (drive letter case, separators,
`\\?\` long-path prefix); this model takes normalised paths as plain strings.

**Cost model.** One unit per `Step`. Each step is either one external command
(`git init`, `gh repo create`, `git clone`) or one persisted state transition
(register, switch, start the interview). Reading the world and computing the plan are
free in this model; they are pure and bounded by the request size.

**Class of alternatives.** Every finite sequence of `Step`s, over the same step
alphabet, that executes from the same world without a refused step and ends in a
world that meets the request's goal (`goal`). `optimal` shows none is shorter than
`plan`'s.
-/

namespace Velocity.NewProject

inductive Visibility where
  | privateRepo | publicRepo
  deriving DecidableEq, Repr

inductive Effort where
  | low | medium | high | max
  deriving DecidableEq, Repr

inductive Speed where
  | regular | fast
  deriving DecidableEq, Repr

/-- `github.com/<owner>/<name>`. -/
structure Repository where
  owner : String
  name : String
  deriving DecidableEq, Repr

/-- The interview model card, effort, speed and the free-text brief. -/
structure InterviewConfig where
  model : String
  effort : Effort
  speed : Speed
  brief : String
  deriving DecidableEq, Repr

inductive Location where
  /-- A local folder. `initIfNeeded` is the "create the folder and run `git init` if
  needed" option. -/
  | localFolder (path : String) (initIfNeeded : Bool)
  /-- GitHub · New repo: create it with the visibility, then clone it to `target`. -/
  | githubNew (repository : Repository) (visibility : Visibility) (target : String)
  /-- GitHub · Existing repo: clone it to `target`. -/
  | githubExisting (repository : Repository) (target : String)
  deriving DecidableEq, Repr

structure Request where
  location : Location
  name : String
  config : InterviewConfig
  deriving DecidableEq, Repr

/-- What the dialog sends: Esc / Cancel, or Create (`Ctrl+Enter`). -/
inductive Response where
  | cancel
  | create (request : Request)

/-- What Velocity observes before it acts. -/
structure World where
  /-- Folders that are git repositories. -/
  repositories : List String
  /-- Folders that exist and are not empty. -/
  occupied : List String
  remotes : List (Repository × Visibility)
  /-- Clones: folder and the GitHub repository it was cloned from. -/
  checkouts : List (String × Repository)
  /-- Registered projects: path and name. A project is identified by its path. -/
  projects : List (String × String)
  active : Option String
  /-- Spec interview threads that were started, per project path. -/
  interviews : List (String × InterviewConfig)

inductive Step where
  /-- `git init <path>`: creates the folder when it is missing. -/
  | gitInit (path : String)
  | createRemote (repository : Repository) (visibility : Visibility)
  | clone (repository : Repository) (path : String)
  | register (path : String) (name : String)
  | switchTo (path : String)
  | startInterview (path : String) (config : InterviewConfig)
  deriving DecidableEq, Repr

inductive Error where
  | notARepository | alreadyAProject | remoteAlreadyExists | remoteMissing | targetOccupied
  deriving DecidableEq, Repr

@[expose] def remoteNames (world : World) : List Repository := world.remotes.map Prod.fst

@[expose] def registered (world : World) : List String := world.projects.map Prod.fst

/-- Executes one step. `none` is a refused step: its precondition does not hold. -/
@[expose] def apply : Step → World → Option World
  | .gitInit path, world =>
      some { world with repositories := path :: world.repositories,
                        occupied := path :: world.occupied }
  | .createRemote repository visibility, world =>
      if repository ∈ remoteNames world then none
      else some { world with remotes := (repository, visibility) :: world.remotes }
  | .clone repository path, world =>
      if repository ∈ remoteNames world ∧ path ∉ world.occupied then
        some { world with repositories := path :: world.repositories,
                          occupied := path :: world.occupied,
                          checkouts := (path, repository) :: world.checkouts }
      else none
  | .register path name, world =>
      if path ∈ registered world then none
      else some { world with projects := (path, name) :: world.projects }
  | .switchTo path, world =>
      if path ∈ registered world then some { world with active := some path } else none
  | .startInterview path config, world =>
      if path ∈ registered world then
        some { world with interviews := (path, config) :: world.interviews }
      else none

@[expose] def execute : List Step → World → Option World
  | [], world => some world
  | step :: remaining, world => (apply step world).bind (execute remaining)

@[expose] def Location.path : Location → String
  | .localFolder path _ => path
  | .githubNew _ _ target => target
  | .githubExisting _ target => target

@[expose] def setupSteps : Location → World → Except Error (List Step)
  | .localFolder path initIfNeeded, world =>
      if path ∈ world.repositories then .ok []
      else if initIfNeeded then .ok [.gitInit path]
      else .error .notARepository
  | .githubNew repository visibility target, world =>
      if repository ∈ remoteNames world then .error .remoteAlreadyExists
      else if target ∈ world.occupied then .error .targetOccupied
      else .ok [.createRemote repository visibility, .clone repository target]
  | .githubExisting repository target, world =>
      if repository ∉ remoteNames world then .error .remoteMissing
      else if target ∈ world.occupied then .error .targetOccupied
      else .ok [.clone repository target]

@[expose] def finishSteps (request : Request) : List Step :=
  [.register request.location.path request.name,
   .switchTo request.location.path,
   .startInterview request.location.path request.config]

@[expose] def plan (request : Request) (world : World) : Except Error (List Step) :=
  if request.location.path ∈ registered world then .error .alreadyAProject
  else (setupSteps request.location world).map (· ++ finishSteps request)

@[expose] def respond : Response → World → Except Error (List Step)
  | .cancel, _ => .ok []
  | .create request, world => plan request world

/-! ## Goal -/

/-- One fact about the world that a request needs to hold at the end. -/
inductive Atom where
  | repository (path : String)
  | remote (repository : Repository) (visibility : Visibility)
  | checkout (path : String) (repository : Repository)
  | project (path : String) (name : String)
  | active (path : String)
  | interview (path : String) (config : InterviewConfig)

@[expose] def holds : Atom → World → Bool
  | .repository path, world => decide (path ∈ world.repositories)
  | .remote repository visibility, world => decide ((repository, visibility) ∈ world.remotes)
  | .checkout path repository, world => decide ((path, repository) ∈ world.checkouts)
  | .project path name, world => decide ((path, name) ∈ world.projects)
  | .active path, world => decide (world.active = some path)
  | .interview path config, world => decide ((path, config) ∈ world.interviews)

@[expose] def setupGoal : Location → List Atom
  | .localFolder path _ => [.repository path]
  | .githubNew repository visibility target =>
      [.remote repository visibility, .checkout target repository]
  | .githubExisting repository target => [.checkout target repository]

/-- F2: the project is set up (a git repository, or a clone of the GitHub repository,
created first for a new repo), registered, switched to, and its spec interview started
with the dialog's model, effort, speed and brief. -/
@[expose] def goal (request : Request) : List Atom :=
  setupGoal request.location ++
    [.project request.location.path request.name,
     .active request.location.path,
     .interview request.location.path request.config]

@[expose] def Meets (request : Request) (world : World) : Prop :=
  ∀ atom ∈ goal request, holds atom world = true

/-! ## Correctness -/

private theorem execute_append (first second : List Step) (world : World) :
    execute (first ++ second) world = (execute first world).bind (execute second) := by
  induction first generalizing world with
  | nil => simp [execute]
  | cons step remaining inductionHypothesis =>
    simp only [List.cons_append, execute]
    cases apply step world <;> simp [inductionHypothesis]

/-- After setup, finishing registers, switches and starts the interview. -/
private theorem finish_runs (request : Request) (world : World)
    (notRegistered : request.location.path ∉ registered world) :
    ∃ final, execute (finishSteps request) world = some final ∧
      holds (.project request.location.path request.name) final = true ∧
      holds (.active request.location.path) final = true ∧
      holds (.interview request.location.path request.config) final = true ∧
      final.repositories = world.repositories ∧ final.remotes = world.remotes ∧
      final.checkouts = world.checkouts := by
  refine ⟨{ world with projects := (request.location.path, request.name) :: world.projects,
                       active := some request.location.path,
                       interviews := (request.location.path, request.config) ::
                         world.interviews }, ?_, ?_⟩
  · simp only [registered] at notRegistered
    simp [finishSteps, execute, apply, registered, notRegistered]
  · simp [holds]

private theorem setup_runs (request : Request) (world : World) (steps : List Step)
    (planned : setupSteps request.location world = .ok steps) :
    ∃ afterSetup, execute steps world = some afterSetup ∧
      afterSetup.projects = world.projects ∧
      ∀ atom ∈ setupGoal request.location, holds atom afterSetup = true := by
  cases hLocation : request.location with
  | localFolder path initIfNeeded =>
    simp only [hLocation, setupSteps] at planned
    by_cases isRepository : path ∈ world.repositories
    · simp only [isRepository, ↓reduceIte, Except.ok.injEq] at planned
      subst planned
      exact ⟨world, rfl, rfl, by simp [setupGoal, holds, isRepository]⟩
    · cases initIfNeeded <;> simp [isRepository] at planned
      subst planned
      exact ⟨_, rfl, rfl, by simp [setupGoal, holds]⟩
  | githubNew repository visibility target =>
    simp only [hLocation, setupSteps] at planned
    by_cases remoteExists : repository ∈ remoteNames world
    · simp [remoteExists] at planned
    · by_cases targetOccupied : target ∈ world.occupied
      · simp [remoteExists, targetOccupied] at planned
      · simp only [remoteExists, targetOccupied, ↓reduceIte, Except.ok.injEq] at planned
        subst planned
        refine ⟨{ world with remotes := (repository, visibility) :: world.remotes,
                             repositories := target :: world.repositories,
                             occupied := target :: world.occupied,
                             checkouts := (target, repository) :: world.checkouts },
          ?_, rfl, ?_⟩
        · simp only [remoteNames] at remoteExists
          simp [execute, apply, remoteNames, remoteExists, targetOccupied]
        · simp [setupGoal, holds]
  | githubExisting repository target =>
    simp only [hLocation, setupSteps] at planned
    by_cases remoteExists : repository ∈ remoteNames world
    · by_cases targetOccupied : target ∈ world.occupied
      · simp [remoteExists, targetOccupied] at planned
      · simp only [remoteExists, targetOccupied, not_true, ↓reduceIte, Except.ok.injEq]
          at planned
        subst planned
        refine ⟨{ world with repositories := target :: world.repositories,
                             occupied := target :: world.occupied,
                             checkouts := (target, repository) :: world.checkouts },
          ?_, rfl, ?_⟩
        · simp only [remoteNames] at remoteExists
          simp [execute, apply, remoteNames, remoteExists, targetOccupied]
        · simp [setupGoal, holds]
    · simp [remoteExists] at planned

private theorem setup_preserves (location : Location) (afterSetup final : World)
    (atom : Atom) (inSetupGoal : atom ∈ setupGoal location)
    (holdsAfter : holds atom afterSetup = true)
    (sameRepositories : final.repositories = afterSetup.repositories)
    (sameRemotes : final.remotes = afterSetup.remotes)
    (sameCheckouts : final.checkouts = afterSetup.checkouts) :
    holds atom final = true := by
  cases location <;> simp only [setupGoal, List.mem_cons, List.not_mem_nil, or_false]
    at inSetupGoal
  all_goals
    (repeat' rcases inSetupGoal with rfl | inSetupGoal) <;>
      simp_all [holds]

/-- SPEC.md F2: Create sets up the project, switches to it and starts its spec
interview. Every planned step runs (none is refused) and the final world meets the
goal. -/
theorem correct (request : Request) (world : World) (steps : List Step)
    (planned : plan request world = .ok steps) :
    ∃ final, execute steps world = some final ∧ Meets request final := by
  simp only [plan] at planned
  by_cases alreadyRegistered : request.location.path ∈ registered world
  · simp [alreadyRegistered] at planned
  · simp only [alreadyRegistered, ↓reduceIte] at planned
    cases hSetup : setupSteps request.location world with
    | error failure => simp [hSetup, Except.map] at planned
    | ok setup =>
      simp only [hSetup, Except.map, Except.ok.injEq] at planned
      subst planned
      obtain ⟨afterSetup, setupRan, sameProjects, setupMet⟩ :=
        setup_runs request world setup hSetup
      have stillUnregistered : request.location.path ∉ registered afterSetup := by
        simpa [registered, sameProjects] using alreadyRegistered
      obtain ⟨final, finishRan, projectMet, activeMet, interviewMet, sameRepositories,
          sameRemotes, sameCheckouts⟩ := finish_runs request afterSetup stillUnregistered
      refine ⟨final, by simp [execute_append, setupRan, finishRan], ?_⟩
      intro atom inGoal
      simp only [goal, List.mem_append, List.mem_cons, List.not_mem_nil, or_false]
        at inGoal
      rcases inGoal with inSetup | rfl | rfl | rfl
      · exact setup_preserves request.location afterSetup final atom inSetup
          (setupMet atom inSetup) sameRepositories sameRemotes sameCheckouts
      · exact projectMet
      · exact activeMet
      · exact interviewMet

/-- SPEC.md F2: Esc cancels and nothing is created: the plan is empty and running it
leaves the world unchanged. -/
theorem cancel_creates_nothing (world : World) :
    respond .cancel world = .ok [] ∧ execute [] world = some world :=
  ⟨rfl, rfl⟩

/-- A refused request runs no step, so it creates nothing either. -/
theorem error_creates_nothing (request : Request) (world : World) (failure : Error)
    (refused : plan request world = .error failure) :
    ∀ steps, plan request world ≠ .ok steps := by
  intro steps planned
  rw [refused] at planned
  cases planned

/-! ## Cost -/

/-- Cost model: one unit per `Step` (one external command or one persisted state
transition). Creating a project costs at most five steps: `gh repo create`, `git clone`,
register, switch, start the interview. -/
theorem cost_bound (request : Request) (world : World) (steps : List Step)
    (planned : plan request world = .ok steps) : steps.length ≤ 5 := by
  simp only [plan] at planned
  split at planned
  · cases planned
  · cases hSetup : setupSteps request.location world with
    | error failure => simp [hSetup, Except.map] at planned
    | ok setup =>
      simp only [hSetup, Except.map, Except.ok.injEq] at planned
      subst planned
      have setupLength : setup.length ≤ 2 := by
        cases hLocation : request.location <;>
          simp only [hLocation, setupSteps] at hSetup <;>
          (repeat' split at hSetup) <;> cases hSetup <;> simp
      simp [finishSteps]
      omega

/-! ## Optimality -/

/-- Which goal atoms a step can make true. -/
@[expose] def establishes : Step → Atom → Bool
  | .gitInit path, .repository goalPath => decide (goalPath = path)
  | .createRemote repository visibility, .remote goalRepository goalVisibility =>
      decide (goalRepository = repository ∧ goalVisibility = visibility)
  | .clone _ path, .repository goalPath => decide (goalPath = path)
  | .clone repository path, .checkout goalPath goalRepository =>
      decide (goalPath = path ∧ goalRepository = repository)
  | .register path name, .project goalPath goalName =>
      decide (goalPath = path ∧ goalName = name)
  | .switchTo path, .active goalPath => decide (goalPath = path)
  | .startInterview path config, .interview goalPath goalConfig =>
      decide (goalPath = path ∧ goalConfig = config)
  | _, _ => false

private theorem only_establishes (step : Step) (world next : World) (atom : Atom)
    (ran : apply step world = some next) (holdsNext : holds atom next = true) :
    establishes step atom = true ∨ holds atom world = true := by
  cases step <;> simp only [apply] at ran <;> (repeat' split at ran) <;>
    (try cases ran) <;> cases atom <;>
    simp_all [holds, establishes]

/-- The number of goal atoms that do not hold yet. -/
@[expose] def deficit (atoms : List Atom) (world : World) : Nat :=
  atoms.countP (fun atom => !holds atom world)

private theorem step_deficit (step : Step) (world next : World) (atoms : List Atom)
    (ran : apply step world = some next) :
    deficit atoms world ≤ deficit atoms next + atoms.countP (establishes step) := by
  induction atoms with
  | nil => simp [deficit]
  | cons atom remaining inductionHypothesis =>
    simp only [deficit, List.countP_cons] at inductionHypothesis ⊢
    have either := only_establishes step world next atom ran
    by_cases holdsNow : holds atom world = true
    · simp [holdsNow]; omega
    · by_cases holdsNext : holds atom next = true
      · have established : establishes step atom = true := by
          rcases either holdsNext with h | h
          · exact h
          · exact absurd h holdsNow
        simp [holdsNow, holdsNext, established]; omega
      · simp [holdsNow, holdsNext]; split <;> omega

/-- No step establishes two atoms of one request's goal. -/
private theorem one_per_step (request : Request) (step : Step) :
    (goal request).countP (establishes step) ≤ 1 := by
  cases hLocation : request.location <;> cases step <;>
    simp only [goal, setupGoal, hLocation, Location.path, List.cons_append,
      List.nil_append, List.countP_cons, List.countP_nil, establishes] <;>
    simp <;> (repeat' split) <;> omega

private theorem lower_bound (request : Request) (alternative : List Step)
    (world final : World) (ran : execute alternative world = some final) :
    deficit (goal request) world ≤ deficit (goal request) final + alternative.length := by
  induction alternative generalizing world with
  | nil => simp [execute] at ran; subst ran; simp
  | cons step remaining inductionHypothesis =>
    simp only [execute] at ran
    cases hStep : apply step world with
    | none => simp [hStep] at ran
    | some next =>
      simp only [hStep, Option.bind_some] at ran
      have stepBound := step_deficit step world next (goal request) hStep
      have perStep := one_per_step request step
      have restBound := inductionHypothesis next ran
      simp only [List.length_cons]
      omega

private theorem met_deficit_zero (request : Request) (world : World)
    (met : Meets request world) : deficit (goal request) world = 0 := by
  simp only [deficit, List.countP_eq_zero]
  intro atom inGoal
  simp [met atom inGoal]

/-- Velocity's invariants on the observed world. Velocity only creates a clone in a
folder it fills, only activates a registered project and only interviews a registered
project; `apply` keeps all three. -/
@[expose] def WellFormed (world : World) : Prop :=
  (∀ path repository, (path, repository) ∈ world.checkouts → path ∈ world.occupied) ∧
  (∀ path, world.active = some path → path ∈ registered world) ∧
  (∀ path config, (path, config) ∈ world.interviews → path ∈ registered world)

theorem apply_wellFormed (step : Step) (world next : World) (wellFormed : WellFormed world)
    (ran : apply step world = some next) : WellFormed next := by
  obtain ⟨checkoutsOccupied, activeRegistered, interviewsRegistered⟩ := wellFormed
  cases step with
  | gitInit path =>
    simp only [apply, Option.some.injEq] at ran
    subst ran
    refine ⟨fun checkoutPath repository inCheckouts => ?_, activeRegistered,
      interviewsRegistered⟩
    exact List.mem_cons_of_mem _ (checkoutsOccupied checkoutPath repository inCheckouts)
  | createRemote repository visibility =>
    simp only [apply] at ran
    split at ran
    · cases ran
    · cases ran
      exact ⟨checkoutsOccupied, activeRegistered, interviewsRegistered⟩
  | clone repository path =>
    simp only [apply] at ran
    split at ran
    · cases ran
      refine ⟨fun checkoutPath checkoutRepository inCheckouts => ?_, activeRegistered,
        interviewsRegistered⟩
      rcases List.mem_cons.mp inCheckouts with isNew | inCheckouts
      · simp only [Prod.mk.injEq] at isNew
        simp [isNew.1]
      · exact List.mem_cons_of_mem _
          (checkoutsOccupied checkoutPath checkoutRepository inCheckouts)
    · cases ran
  | register path name =>
    simp only [apply] at ran
    split at ran
    · cases ran
    · cases ran
      refine ⟨checkoutsOccupied, fun activePath isActive => ?_,
        fun interviewPath config inInterviews => ?_⟩
      · simp only [registered, List.map_cons, List.mem_cons]
        exact Or.inr (activeRegistered activePath isActive)
      · simp only [registered, List.map_cons, List.mem_cons]
        exact Or.inr (interviewsRegistered interviewPath config inInterviews)
  | switchTo path =>
    simp only [apply] at ran
    split at ran
    · cases ran
      refine ⟨checkoutsOccupied, fun activePath isActive => ?_, interviewsRegistered⟩
      simp only [Option.some.injEq] at isActive
      subst isActive
      assumption
    · cases ran
  | startInterview path config =>
    simp only [apply] at ran
    split at ran
    · cases ran
      refine ⟨checkoutsOccupied, activeRegistered,
        fun interviewPath interviewConfig inInterviews => ?_⟩
      rcases List.mem_cons.mp inInterviews with isNew | inInterviews
      · simp only [Prod.mk.injEq] at isNew
        rw [isNew.1]
        assumption
      · exact interviewsRegistered interviewPath interviewConfig inInterviews
    · cases ran

/-- Every planned step makes exactly one goal atom true: the plan's length is the
number of goal atoms that do not hold in the starting world. -/
private theorem plan_length (request : Request) (world : World) (steps : List Step)
    (wellFormed : WellFormed world) (planned : plan request world = .ok steps) :
    steps.length = deficit (goal request) world := by
  obtain ⟨checkoutsOccupied, activeRegistered, interviewsRegistered⟩ := wellFormed
  simp only [plan] at planned
  by_cases alreadyRegistered : request.location.path ∈ registered world
  · simp [alreadyRegistered] at planned
  · simp only [alreadyRegistered, ↓reduceIte] at planned
    have notProject : (request.location.path, request.name) ∉ world.projects := by
      intro inProjects
      exact alreadyRegistered (List.mem_map.mpr ⟨_, inProjects, rfl⟩)
    have notActive : world.active ≠ some request.location.path := by
      intro isActive
      exact alreadyRegistered (activeRegistered _ isActive)
    have notInterviewed : (request.location.path, request.config) ∉ world.interviews := by
      intro inInterviews
      exact alreadyRegistered (interviewsRegistered _ _ inInterviews)
    cases hSetup : setupSteps request.location world with
    | error failure => simp [hSetup, Except.map] at planned
    | ok setup =>
      simp only [hSetup, Except.map, Except.ok.injEq] at planned
      subst planned
      cases hLocation : request.location with
      | localFolder path initIfNeeded =>
        simp only [hLocation, Location.path] at notProject notActive notInterviewed
        simp only [hLocation, setupSteps] at hSetup
        by_cases isRepository : path ∈ world.repositories
        · simp [isRepository] at hSetup
          subst hSetup
          simp [goal, setupGoal, hLocation, Location.path, deficit, holds, finishSteps,
            isRepository, notProject, notActive, notInterviewed]
        · cases initIfNeeded <;> simp [isRepository] at hSetup
          subst hSetup
          simp [goal, setupGoal, hLocation, Location.path, deficit, holds, finishSteps,
            isRepository, notProject, notActive, notInterviewed]
      | githubNew repository visibility target =>
        simp only [hLocation, Location.path] at notProject notActive notInterviewed
        simp only [hLocation, setupSteps] at hSetup
        by_cases remoteExists : repository ∈ remoteNames world
        · simp [remoteExists] at hSetup
        · by_cases targetOccupied : target ∈ world.occupied
          · simp [remoteExists, targetOccupied] at hSetup
          · simp [remoteExists, targetOccupied] at hSetup
            subst hSetup
            have notRemote : (repository, visibility) ∉ world.remotes := by
              intro inRemotes
              exact remoteExists (List.mem_map.mpr ⟨_, inRemotes, rfl⟩)
            have notCheckout : (target, repository) ∉ world.checkouts := by
              intro inCheckouts
              exact targetOccupied (checkoutsOccupied _ _ inCheckouts)
            simp [goal, setupGoal, hLocation, Location.path, deficit, holds, finishSteps,
              notRemote, notCheckout, notProject, notActive, notInterviewed]
      | githubExisting repository target =>
        simp only [hLocation, Location.path] at notProject notActive notInterviewed
        simp only [hLocation, setupSteps] at hSetup
        by_cases remoteExists : repository ∈ remoteNames world
        · by_cases targetOccupied : target ∈ world.occupied
          · simp [remoteExists, targetOccupied] at hSetup
          · simp [remoteExists, targetOccupied] at hSetup
            subst hSetup
            have notCheckout : (target, repository) ∉ world.checkouts := by
              intro inCheckouts
              exact targetOccupied (checkoutsOccupied _ _ inCheckouts)
            simp [goal, setupGoal, hLocation, Location.path, deficit, holds, finishSteps,
              notCheckout, notProject, notActive, notInterviewed]
        · simp [remoteExists] at hSetup

/-- Optimality. Cost model: one unit per `Step`. Class of alternatives: every finite
`Step` sequence that runs from the same well-formed world with no refused step and
ends in a world meeting the request's goal. None is shorter than the plan. -/
theorem optimal (request : Request) (world : World) (steps : List Step)
    (wellFormed : WellFormed world) (planned : plan request world = .ok steps)
    (alternative : List Step) (final : World)
    (ran : execute alternative world = some final) (met : Meets request final) :
    steps.length ≤ alternative.length := by
  have planLength := plan_length request world steps wellFormed planned
  have bound := lower_bound request alternative world final ran
  have zero := met_deficit_zero request final met
  omega

#print axioms correct
#print axioms cancel_creates_nothing
#print axioms error_creates_nothing
#print axioms cost_bound
#print axioms apply_wellFormed
#print axioms optimal

end Velocity.NewProject
