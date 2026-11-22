"""Check F16 reopened-issue eligibility at inventory and claim time."""
from pathlib import Path
import sys

root = Path.cwd()
def read(path):
    return (root / path).read_text(encoding="utf-8")

pool = read("crates/velocity-threads/src/work_pool.rs").split("fn issue_pieces(", 1)[1].split("fn held_rosen_issues(", 1)[0]
binding = read("crates/velocity-store/src/goal_swarms.rs").split("fn bind_piece_owner(", 1)[1].split("WorkKey::Issue", 1)[1].split("WorkKey::Spec", 1)[0]
work = read("crates/velocity-store/src/rosen_work.rs")
reserve = read("crates/velocity-threads/src/goal_swarm.rs")

if ("IssueVersion::from_github(issue)" in pool
        and "rosen_issue_completed_for_version" in pool
        and "rosen_issue_completed(&issue)" in binding
        and "IssueCompleted" in binding and "IssueCompleted" in reserve
        and "IssueVersion::default()" in work):
    print("FAIL: issue_pieces admits a reopened issue using its current version, "
          "but bind_piece_owner uses the default version and rejects it as "
          "IssueCompleted. A Goal Swarm can advertise available work and "
          "leave after its claim is refused.", file=sys.stderr)
    sys.exit(1)
