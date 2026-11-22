"""Check whether the F16 reference fixtures exercise projected work eligibility."""
from pathlib import Path
import sys

root = Path.cwd()
design = (root / "proofs/GoalSwarmWorkChoice/Design.lean").read_text(encoding="utf-8")
fixtures = (root / "proofs/fixtures/goal-swarm-work-choice.tsv").read_text(encoding="utf-8")
fixture_name = "goal-swarm-work-choice.tsv"
kinds = {row.split("\t", 1)[0] for row in fixtures.splitlines() if row.strip()}
for relative in ("crates/velocity-core", "crates/velocity-store", "crates/velocity-threads"):
    for path in (root / relative).rglob("*.rs"):
        if fixture_name in path.read_text(encoding="utf-8"):
            print("Fixture reader:", path.relative_to(root))
print("Fixture cases:", ", ".join(sorted(kinds)))

if ("(board : List Bool)" in design and "projected" not in design.lower()
        and kinds == {"choose", "attempt", "merge"}):
    print("FAIL: F16 takes an already open/closed board. Its fixtures cannot "
          "distinguish a Todo task with a detached Landed projection from an "
          "ordinary Todo task or exercise the persisted inventory and claim paths.",
          file=sys.stderr)
    sys.exit(1)

