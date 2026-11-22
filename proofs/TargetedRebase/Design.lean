module
public import Mathlib.Data.Finset.Card
public section
namespace TargetedRebase
-- SPEC.md F6/F7/F14. Native edges supply overlap, preflight attestations, bindings
-- touch evidence and Rosen's current review/recheck policy.
def affected (claimed edited contested : Bool) : Bool := contested && (claimed || edited)
def selected (live : Finset Nat) (overlap : Nat → Bool) : Finset Nat :=
  live.filter (fun agent => overlap agent = true)

-- Preserved legacy check-scope model until the proof's scope is explicitly reconciled (#629).
def checks (sameHead sameBase overlap : Bool) : Nat :=
  if !sameHead then 2 else if sameBase then 0 else if overlap then 2 else 1
def EnoughChecks (sameHead sameBase overlap : Bool) (passes : Nat) : Prop :=
  (sameHead = false → 2 ≤ passes) ∧ (sameBase = false → 1 ≤ passes) ∧
  (sameBase = false → overlap = true → 2 ≤ passes)

-- Live F6/F7 Rosen pre-land check gate (landing_runner.rs, bindings_gate.rs, #503, #560):
-- refuse without staging a merge check when preflight is not current; stage one
-- `export-bindings --check` pass when preflight is current and bindings are touched
-- and unverified; otherwise land with zero staged merge checks.
inductive PreLandAction where
  | refusePreflight
  | checkBindings
  | land
  deriving DecidableEq

def preLandPlan (preflightCurrent touchesBindings bindingsVerified : Bool) : PreLandAction :=
  if !preflightCurrent then .refusePreflight
  else if touchesBindings && !bindingsVerified then .checkBindings
  else .land

def preLandPasses (preflightCurrent touchesBindings bindingsVerified : Bool) : Nat :=
  match preLandPlan preflightCurrent touchesBindings bindingsVerified with
  | .checkBindings => 1
  | .refusePreflight | .land => 0

def EnoughPreLandChecks (preflightCurrent touchesBindings bindingsVerified : Bool)
    (passes : Nat) : Prop :=
  preflightCurrent = true → touchesBindings = true → bindingsVerified = false → 1 ≤ passes

-- Live F7 Rosen post-land check scope (velocity-rosen/src/post_land.rs, #567):
-- every landed batch runs a test/integration pass (1); before-land projects with merge
-- rechecking or landing-priority builds add a separate build-prefix pass (2), whereas
-- after-land folds build/lint into the single integration pass (prepending `pre_land`
-- commands whenever `post_land` does not already run the full suite).
def postLandSeparateBuild (afterLand buildPrefixRequired : Bool) : Bool :=
  !afterLand && buildPrefixRequired

def postLandPrependPreLand (afterLand hasFullSuite : Bool) : Bool :=
  afterLand && !hasFullSuite

def postLandPasses (afterLand buildPrefixRequired : Bool) : Nat :=
  if postLandSeparateBuild afterLand buildPrefixRequired then 2 else 1

def EnoughPostLandChecks (afterLand buildPrefixRequired : Bool) (passes : Nat) : Prop :=
  1 ≤ passes ∧ (afterLand = false → buildPrefixRequired = true → 2 ≤ passes)

def Spec (live : Finset Nat) (overlap : Nat → Bool) (sameHead sameBase touch : Bool) : Prop :=
  (∀ agent, agent ∈ selected live overlap ↔ agent ∈ live ∧ overlap agent = true) ∧
  EnoughChecks sameHead sameBase touch (checks sameHead sameBase touch) ∧
  (∀ preflightCurrent touchesBindings bindingsVerified,
    EnoughPreLandChecks preflightCurrent touchesBindings bindingsVerified
      (preLandPasses preflightCurrent touchesBindings bindingsVerified) ∧
    (preLandPlan preflightCurrent touchesBindings bindingsVerified = .refusePreflight ↔
      preflightCurrent = false) ∧
    (preLandPlan preflightCurrent touchesBindings bindingsVerified = .checkBindings ↔
      preflightCurrent = true ∧ touchesBindings = true ∧ bindingsVerified = false) ∧
    (preLandPlan preflightCurrent touchesBindings bindingsVerified = .land ↔
      preflightCurrent = true ∧ (touchesBindings = false ∨ bindingsVerified = true))) ∧
  (∀ afterLand buildPrefixRequired hasFullSuite,
    EnoughPostLandChecks afterLand buildPrefixRequired
      (postLandPasses afterLand buildPrefixRequired) ∧
    (postLandSeparateBuild afterLand buildPrefixRequired = true ↔
      afterLand = false ∧ buildPrefixRequired = true) ∧
    (postLandPrependPreLand afterLand hasFullSuite = true ↔
      afterLand = true ∧ hasFullSuite = false))

theorem correct (live : Finset Nat) (overlap : Nat → Bool) (sameHead sameBase touch : Bool) :
    Spec live overlap sameHead sameBase touch := by
  refine ⟨?_, ?_, ?_, ?_⟩
  · intro agent; simp [selected]
  · cases sameHead <;> cases sameBase <;> cases touch <;> simp [EnoughChecks, checks]
  · intro preflightCurrent touchesBindings bindingsVerified
    cases preflightCurrent <;> cases touchesBindings <;> cases bindingsVerified <;>
      simp [EnoughPreLandChecks, preLandPasses, preLandPlan]
  · intro afterLand buildPrefixRequired hasFullSuite
    cases afterLand <;> cases buildPrefixRequired <;> cases hasFullSuite <;>
      simp [EnoughPostLandChecks, postLandPasses, postLandSeparateBuild, postLandPrependPreLand]

-- Cost counts branch updates and check passes. Discovery/network costs are external.
def cost (agents : Finset Nat) (passes : Nat) : Nat := agents.card + passes

-- Live Rosen check cost counts staged pre-land merge checks plus post-land verification passes.
def rosenCheckCost (preflightCurrent touchesBindings bindingsVerified
    afterLand buildPrefixRequired : Bool) : Nat :=
  preLandPasses preflightCurrent touchesBindings bindingsVerified +
  postLandPasses afterLand buildPrefixRequired

theorem cost_bound (live : Finset Nat) (overlap : Nat → Bool) (sameHead sameBase touch : Bool) :
    cost (selected live overlap) (checks sameHead sameBase touch) ≤ live.card + 2 ∧
    (∀ preflightCurrent touchesBindings bindingsVerified afterLand buildPrefixRequired,
      preLandPasses preflightCurrent touchesBindings bindingsVerified ≤ 1 ∧
      postLandPasses afterLand buildPrefixRequired ≤ (if afterLand then 1 else 2) ∧
      rosenCheckCost preflightCurrent touchesBindings bindingsVerified afterLand buildPrefixRequired ≤
        (if afterLand then 2 else 3)) := by
  constructor
  · have bound : (selected live overlap).card ≤ live.card := Finset.card_le_card (Finset.filter_subset _ _)
    cases sameHead <;> cases sameBase <;> cases touch <;> simp_all [cost, checks] <;> omega
  · intro preflightCurrent touchesBindings bindingsVerified afterLand buildPrefixRequired
    cases preflightCurrent <;> cases touchesBindings <;> cases bindingsVerified <;>
      cases afterLand <;> cases buildPrefixRequired <;>
      simp [rosenCheckCost, preLandPasses, preLandPlan, postLandPasses, postLandSeparateBuild]

-- Alternatives may add updates/checks but must cover affected agents, stale code,
-- required pre-land bindings checks, and required post-land build/test passes.
theorem optimal (live : Finset Nat) (overlap : Nat → Bool)
    (sameHead sameBase touch : Bool) (alternative : Finset Nat) (passes : Nat)
    (covers : ∀ agent ∈ live, overlap agent = true → agent ∈ alternative)
    (enough : EnoughChecks sameHead sameBase touch passes) :
    cost (selected live overlap) (checks sameHead sameBase touch) ≤ cost alternative passes ∧
    (∀ preflightCurrent touchesBindings bindingsVerified afterLand buildPrefixRequired
      (altPreLand altPostLand : Nat),
      EnoughPreLandChecks preflightCurrent touchesBindings bindingsVerified altPreLand →
      EnoughPostLandChecks afterLand buildPrefixRequired altPostLand →
      rosenCheckCost preflightCurrent touchesBindings bindingsVerified afterLand buildPrefixRequired ≤
        altPreLand + altPostLand) := by
  constructor
  · have subset : selected live overlap ⊆ alternative := by
      intro agent member
      have parts := Finset.mem_filter.mp member
      exact covers agent parts.1 parts.2
    have bound := Finset.card_le_card subset
    cases sameHead <;> cases sameBase <;> cases touch <;> simp_all [EnoughChecks, cost, checks] <;> omega
  · intro preflightCurrent touchesBindings bindingsVerified afterLand buildPrefixRequired
      altPreLand altPostLand enoughPre enoughPost
    cases preflightCurrent <;> cases touchesBindings <;> cases bindingsVerified <;>
      cases afterLand <;> cases buildPrefixRequired <;>
      simp_all [rosenCheckCost, EnoughPreLandChecks, EnoughPostLandChecks,
        preLandPasses, preLandPlan, postLandPasses, postLandSeparateBuild] <;> omega

#print axioms correct
#print axioms cost_bound
#print axioms optimal

def preLandActionName : PreLandAction → String
  | .refusePreflight => "refuse_preflight"
  | .checkBindings => "check_bindings"
  | .land => "land"

def rebaseFixtures : String := String.join ([false, true].flatMap fun claimed =>
  [false, true].flatMap fun edited => [false, true].map fun contested =>
    s!"rebase\t{claimed}\t{edited}\t{contested}\t{affected claimed edited contested}\n")

def preLandFixtures : String := String.join ([false, true].flatMap fun preflightCurrent =>
  [false, true].flatMap fun touchesBindings => [false, true].map fun bindingsVerified =>
    let action := preLandPlan preflightCurrent touchesBindings bindingsVerified
    let passes := preLandPasses preflightCurrent touchesBindings bindingsVerified
    s!"pre_land\t{preflightCurrent}\t{touchesBindings}\t{bindingsVerified}\t{preLandActionName action}\t{passes}\n")

def postLandFixtures : String := String.join ([false, true].flatMap fun afterLand =>
  [false, true].flatMap fun buildPrefixRequired => [false, true].map fun hasFullSuite =>
    let sep := postLandSeparateBuild afterLand buildPrefixRequired
    let prepend := postLandPrependPreLand afterLand hasFullSuite
    let passes := postLandPasses afterLand buildPrefixRequired
    s!"post_land\t{afterLand}\t{buildPrefixRequired}\t{hasFullSuite}\t{sep}\t{prepend}\t{passes}\n")

def fixtures : String := rebaseFixtures ++ preLandFixtures ++ postLandFixtures
#eval IO.FS.writeFile "fixtures/targeted-rebase.tsv" fixtures
end TargetedRebase
