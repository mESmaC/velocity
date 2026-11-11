module
public import Std
public section

/-!
SPEC.md F5/F12, F17 worktree deletion only. The I/O edge verifies the saved
thread branch, registered path and common repository, stops the agent and reads
remote reachability before evaluating this guard. Unknown facts fail closed.
Paths are opaque: a removal command always names the supplied owning path.
Cost counts worktree removal commands; git queries, branch cleanup and retries
are external. Alternatives are command lists that perform every required removal.
-/
namespace Velocity.WorktreeManager

def deletionAllowed (merged threadDeleted owned unpushed : Bool) : Bool :=
  (merged || threadDeleted) && owned && !unpushed

def removals (path : Path) (merged threadDeleted owned unpushed : Bool) : List Path :=
  if deletionAllowed merged threadDeleted owned unpushed then [path] else []

-- Eligibility is stated independently of the executable Boolean guard.
def Eligible (merged threadDeleted owned unpushed : Bool) : Prop :=
  (merged = true \/ threadDeleted = true) /\ owned = true /\ unpushed = false

/-- Exactly the F5/F12 events permit removal, with no unpushed work and verified
ownership. Every emitted removal names this thread's supplied path. -/
theorem correct (path : Path) (merged threadDeleted owned unpushed : Bool) :
    (removals path merged threadDeleted owned unpushed = [path] <->
      Eligible merged threadDeleted owned unpushed) /\
    (forall target, target ∈ removals path merged threadDeleted owned unpushed ->
      target = path /\ owned = true /\ unpushed = false) := by
  cases merged <;> cases threadDeleted <;> cases owned <;> cases unpushed <;>
    simp [removals, deletionAllowed, Eligible]

/-- Cost is the number of worktree removal commands, bounded by one per request. -/
theorem cost_bound (path : Path) (merged threadDeleted owned unpushed : Bool) :
    (removals path merged threadDeleted owned unpushed).length <= 1 := by
  unfold removals
  split <;> simp

/-- Among all command lists that remove the owning path whenever F5/F12 requires
it, no alternative uses fewer worktree removal commands. -/
theorem optimal (path : Path) (merged threadDeleted owned unpushed : Bool)
    (alternative : List Path)
    (complete : Eligible merged threadDeleted owned unpushed -> path ∈ alternative) :
    (removals path merged threadDeleted owned unpushed).length <= alternative.length := by
  by_cases eligible : Eligible merged threadDeleted owned unpushed
  . have planned := ((correct path merged threadDeleted owned unpushed).1).mpr eligible
    have present := complete eligible
    cases alternative with
    | nil => simp at present
    | cons first rest => simp [planned]
  . cases merged <;> cases threadDeleted <;> cases owned <;> cases unpushed <;>
      simp_all [Eligible, removals, deletionAllowed]

#print axioms correct
#print axioms cost_bound
#print axioms optimal

-- Export every Boolean input combination, including unsafe deletion events.
def fixtures : String := String.join ([false, true].flatMap fun merged =>
  [false, true].flatMap fun deleted => [false, true].flatMap fun owned =>
    [false, true].map fun unpushed =>
      s!"{merged}\t{deleted}\t{owned}\t{unpushed}\t{deletionAllowed merged deleted owned unpushed}\n")
#eval IO.FS.writeFile "fixtures/worktree_deletion.tsv" fixtures
end Velocity.WorktreeManager
