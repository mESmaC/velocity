"""Check the first Rosen Goal Swarm claim, before finalizing the grant."""
from pathlib import Path
import re

root = Path.cwd()
goal = (root / "crates/velocity-threads/src/goal_swarm.rs").read_text(encoding="utf-8")
remote = (root / "crates/velocity-airc/src/remote_claims.rs").read_text(encoding="utf-8")
try_claim = goal.split("    fn try_claim(", 1)[1].split("\n    fn ", 1)[0]
preclaim = re.search(
    r"self\.channel\.claim_goal_piece\(\s*&self\.member,\s*&piece\.key\.claim_piece\(\),"
    r"\s*&piece\.title,\s*self\.initial_branch,\s*candidate\.files\.clone\(\),", try_claim)
if preclaim and "claim_work_item" not in try_claim:
    assert "client.claim_paths(request)" in remote
    print("Goal Swarm first claims through claim_goal_piece with initial_branch and candidate.files.")
    print("AIRC's Rosen adapter sends an exclusive path claim for eligible files.")
    raise AssertionError("The initial Rosen Goal Swarm claim must carry work-item identity only.")
