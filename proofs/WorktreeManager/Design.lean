module
public import Std
public section

/-!
SPEC.md F5/F12, F17 workspace deletion. The git edge verifies ownership and
remote reachability; unknown facts fail closed. F8 (#514) adds a Velocity gate
before dispatch asks Rosen to abandon finished work. Rosen itself still checks
ownership and stored edits before removing any workspace (F5, Rosen R4).
Cost counts requests for removal in one inspection of an eligible item.
Alternatives are command lists that issue each required request on that pass.
-/
namespace Velocity.WorktreeManager

def deletionAllowed (merged threadDeleted owned unpushed : Bool) : Bool :=
  (merged || threadDeleted) && owned && !unpushed

def removals (path : Path) (merged threadDeleted owned unpushed : Bool) : List Path :=
  if deletionAllowed merged threadDeleted owned unpushed then [path] else []

-- Eligibility is stated independently of the executable Boolean guard.
def Eligible (merged threadDeleted owned unpushed : Bool) : Prop :=
  (merged = true \/ threadDeleted = true) /\ owned = true /\ unpushed = false

inductive CleanupPhase where
  | absent | setup | selecting | running | ended | handoff
  deriving Repr

-- Handoff is the closing runtime's boundary mode. `blocked` means a shutdown
-- or workspace update is in flight, and `pending` means an input or AIRC wake.
def atBoundary (phase : CleanupPhase) (blocked busy queued pending : Bool) : Bool :=
  !blocked && match phase with
  | .absent => true
  | .setup | .selecting => false
  | .running => !queued && !busy && !pending
  | .ended => !queued
  | .handoff => !busy

def CanAbandon (phase : CleanupPhase) (blocked busy queued pending : Bool) : Prop :=
  blocked = false /\ (phase = .absent \/
    (phase = .running /\ queued = false /\ busy = false /\ pending = false) \/
    (phase = .ended /\ queued = false) \/ (phase = .handoff /\ busy = false))

def abandonRequests (change : String) (phase : CleanupPhase)
    (blocked busy queued pending : Bool) : List String :=
  if atBoundary phase blocked busy queued pending then [change] else []

/-- Exactly the F5/F12 events permit removal, with no unpushed work and verified
ownership. Every emitted removal names this thread's supplied path. The F8
dispatch boundary separately controls requests to Rosen for a finished change. -/
theorem correct (path : Path) (merged threadDeleted owned unpushed : Bool) :
    (removals path merged threadDeleted owned unpushed = [path] <->
      Eligible merged threadDeleted owned unpushed) /\
    (forall target, target ∈ removals path merged threadDeleted owned unpushed ->
      target = path /\ owned = true /\ unpushed = false) /\
    (forall (change : String) phase blocked busy queued pending,
      abandonRequests change phase blocked busy queued pending = [change] <->
        CanAbandon phase blocked busy queued pending) := by
  constructor
  . cases merged <;> cases threadDeleted <;> cases owned <;> cases unpushed <;>
      simp [removals, deletionAllowed, Eligible]
  constructor
  . cases merged <;> cases threadDeleted <;> cases owned <;> cases unpushed <;>
      simp [removals, deletionAllowed]
  . intro change phase blocked busy queued pending
    cases phase <;> cases blocked <;> cases busy <;> cases queued <;> cases pending <;>
      simp [abandonRequests, atBoundary, CanAbandon]

/-- Cost is the number of git removal commands or Rosen abandon requests,
bounded by one per item per inspection. -/
theorem cost_bound (path : Path) (merged threadDeleted owned unpushed : Bool) :
    (removals path merged threadDeleted owned unpushed).length <= 1 /\
    (forall (change : String) phase blocked busy queued pending,
      (abandonRequests change phase blocked busy queued pending).length <= 1) := by
  constructor
  . unfold removals
    split <;> simp
  . intro change phase blocked busy queued pending
    unfold abandonRequests
    split <;> simp

/-- Among command lists that request the owning path's removal whenever F5/F12
requires it, or request abandonment when F8 permits cleanup of a finished
change, these plans minimize the number of commands per inspection. -/
theorem optimal (path : Path) (merged threadDeleted owned unpushed : Bool)
    (alternative : List Path)
    (complete : Eligible merged threadDeleted owned unpushed -> path ∈ alternative) :
    (removals path merged threadDeleted owned unpushed).length <= alternative.length /\
    (forall (change : String) phase blocked busy queued pending (other : List String),
      (CanAbandon phase blocked busy queued pending -> change ∈ other) ->
        (abandonRequests change phase blocked busy queued pending).length <= other.length) := by
  constructor
  . by_cases eligible : Eligible merged threadDeleted owned unpushed
    . have planned := ((correct path merged threadDeleted owned unpushed).1).mpr eligible
      have present := complete eligible
      cases alternative with
      | nil => simp at present
      | cons first rest => simp [planned]
    . cases merged <;> cases threadDeleted <;> cases owned <;> cases unpushed <;>
        simp_all [Eligible, removals, deletionAllowed]
  . intro change phase blocked busy queued pending other completeAbandon
    have agrees := (correct path merged threadDeleted owned unpushed).2.2
      change phase blocked busy queued pending
    unfold abandonRequests at agrees ⊢
    split <;> rename_i canRequest
    . have present := completeAbandon (agrees.mp (by simp [canRequest]))
      cases other with
      | nil => simp at present
      | cons first rest => simp
    . simp

#print axioms correct
#print axioms cost_bound
#print axioms optimal

-- Export every Boolean input combination, including unsafe deletion events.
def fixtures : String := String.join ([false, true].flatMap fun merged =>
  [false, true].flatMap fun deleted => [false, true].flatMap fun owned =>
    [false, true].map fun unpushed =>
      s!"{merged}\t{deleted}\t{owned}\t{unpushed}\t{deletionAllowed merged deleted owned unpushed}\n")
#eval IO.FS.writeFile "fixtures/worktree_deletion.tsv" fixtures

def dispatchFixtures : String :=
  let phases : List (CleanupPhase × String) := [(.absent, "absent"), (.setup, "setup"),
    (.selecting, "selecting"), (.running, "running"), (.ended, "ended"), (.handoff, "handoff")]
  let bits := [false, true]
  String.join (phases.flatMap fun (phase, name) => bits.flatMap fun blocked =>
    bits.flatMap fun busy => bits.flatMap fun queued => bits.map fun pending =>
      s!"{name}\t{blocked}\t{busy}\t{queued}\t{pending}\t{atBoundary phase blocked busy queued pending}\n")
#eval IO.FS.writeFile "fixtures/dispatch_cleanup.tsv" dispatchFixtures
end Velocity.WorktreeManager
