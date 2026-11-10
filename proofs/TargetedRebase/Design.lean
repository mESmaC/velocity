module
public import Mathlib.Data.Finset.Card
public import Mathlib.Tactic
public section
namespace TargetedRebase
-- SPEC.md F6/F14. Native edges supply overlap and exact head/base identities.
def affected (claimed edited contested : Bool) : Bool := contested && (claimed || edited)
def selected (live : Finset Nat) (overlap : Nat → Bool) : Finset Nat :=
  live.filter (fun agent => overlap agent = true)
-- One pass is build+lint; the second comprises the remaining local checks.
def checks (sameHead sameBase overlap : Bool) : Nat :=
  if !sameHead then 2 else if sameBase then 0 else if overlap then 2 else 1
def EnoughChecks (sameHead sameBase overlap : Bool) (passes : Nat) : Prop :=
  (sameHead = false → 2 ≤ passes) ∧ (sameBase = false → 1 ≤ passes) ∧
  (sameBase = false → overlap = true → 2 ≤ passes)
def Spec (live : Finset Nat) (overlap : Nat → Bool) (sameHead sameBase touch : Bool) : Prop :=
  (∀ agent, agent ∈ selected live overlap ↔ agent ∈ live ∧ overlap agent = true) ∧
  EnoughChecks sameHead sameBase touch (checks sameHead sameBase touch)

theorem correct (live : Finset Nat) (overlap : Nat → Bool) (sameHead sameBase touch : Bool) :
    Spec live overlap sameHead sameBase touch := by
  constructor
  · intro agent; simp [selected]
  · cases sameHead <;> cases sameBase <;> cases touch <;> simp [EnoughChecks, checks]

-- Cost counts branch updates and check passes. Discovery/network costs are external.
def cost (agents : Finset Nat) (passes : Nat) : Nat := agents.card + passes
theorem cost_bound (live : Finset Nat) (overlap : Nat → Bool) (sameHead sameBase touch : Bool) :
    cost (selected live overlap) (checks sameHead sameBase touch) ≤ live.card + 2 := by
  have bound : (selected live overlap).card ≤ live.card := Finset.card_le_card (Finset.filter_subset _ _)
  cases sameHead <;> cases sameBase <;> cases touch <;> simp_all [cost, checks] <;> omega

-- Alternatives may add updates/checks but must cover affected agents and stale code.
theorem optimal (live : Finset Nat) (overlap : Nat → Bool)
    (sameHead sameBase touch : Bool) (alternative : Finset Nat) (passes : Nat)
    (covers : ∀ agent ∈ live, overlap agent = true → agent ∈ alternative)
    (enough : EnoughChecks sameHead sameBase touch passes) :
    cost (selected live overlap) (checks sameHead sameBase touch) ≤ cost alternative passes := by
  have subset : selected live overlap ⊆ alternative := by
    intro agent member
    have parts := Finset.mem_filter.mp member
    exact covers agent parts.1 parts.2
  have bound := Finset.card_le_card subset
  cases sameHead <;> cases sameBase <;> cases touch <;> simp_all [EnoughChecks, cost, checks] <;> omega

#print axioms correct
#print axioms cost_bound
#print axioms optimal
def fixtures : String := String.join ([false, true].flatMap fun first =>
  [false, true].flatMap fun second => [false, true].map fun third =>
    s!"{first}\t{second}\t{third}\t{affected first second third}\t{checks first second third}\n")
#eval IO.FS.writeFile "fixtures/targeted-rebase.tsv" fixtures
end TargetedRebase
