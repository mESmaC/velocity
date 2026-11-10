module
public import Std
public section

-- SPEC.md F14: serialized first-wins ownership decisions. The channel lock
-- linearizes read/decide/append; durability and scheduling are outside this model.
namespace ClaimLock

def allowed (holder : Option Nat) (requester : Nat) : Bool :=
  match holder with
  | none => true
  | some owner => owner == requester

def acquire (holder : Option Nat) (requester : Nat) : Option Nat :=
  match holder with
  | none => some requester
  | some owner => some owner

def Spec (decision : Option Nat -> Nat -> Bool) : Prop :=
  And (forall requester, decision none requester = true)
      (forall owner requester, decision (some owner) requester = (owner == requester))

theorem correct : And (Spec allowed)
    (And (forall requester, acquire none requester = some requester)
         (forall owner requester, acquire (some owner) requester = some owner)) := by
  constructor
  . constructor <;> intros <;> rfl
  . constructor <;> intros <;> rfl

-- Cost counts ownership-cell observations per decision, excluding log replay
-- and overlap enumeration. Rust observes the current holder once.
def observations : Nat := 1

theorem cost_bound : forall (_holder : Option Nat) (_requester : Nat),
    Spec allowed -> observations <= 1 := by
  intros
  exact Nat.le_refl 1

-- Alternatives are exact deterministic decisions with a fixed worst-case
-- ownership observation budget. Zero observations cannot distinguish holders.
theorem optimal (decision : Option Nat -> Nat -> Bool) (budget : Nat)
    (valid : Spec decision)
    (noObservation : budget = 0 -> forall holder requester,
      decision holder requester = decision none requester) :
    observations <= budget := by
  cases budget with
  | zero =>
    have same := noObservation rfl (some 1) 0
    rw [valid.1 0, valid.2 1 0] at same
    contradiction
  | succ budget => exact Nat.succ_le_succ (Nat.zero_le budget)

#print axioms correct
#print axioms cost_bound
#print axioms optimal

-- Decision equivalence classes: absent, same owner, different owner.
def fixtureLine (holder : Option Nat) (requester : Nat) : String :=
  let label := holder.map toString |>.getD "-"
  s!"{label}\t{requester}\t{allowed holder requester}\n"

def fixtures : String :=
  fixtureLine none 7 ++ fixtureLine (some 7) 7 ++ fixtureLine (some 8) 7
#eval IO.FS.writeFile "fixtures/claim-lock.tsv" fixtures
end ClaimLock
