# F5 Rosen Base CI (#507): a failed check-run-only result must reach the watcher.
# This source-level transport probe uses a commit with a check from a GitHub App,
# no Actions workflows, and no legacy commit statuses. It needs no GitHub login.
from pathlib import Path
import re

watcher = Path("crates/velocity-github/src/base_ci.rs").read_text(encoding="utf-8")
pin_tracker = Path("crates/velocity-github/src/rosen_pin.rs").read_text(encoding="utf-8")
commit = "a" * 40
actions = {"total_count": 0, "workflow_runs": []}
statuses = {"sha": commit, "total_count": 0, "statuses": []}
checks = {"total_count": 1, "check_runs": [{
    "head_sha": commit,
    "app": {"id": 24, "slug": "external-ci"},
    "name": "macOS tests",
    "status": "completed",
    "conclusion": "failure",
}]}

commit_checks = r'"[^"\n]*/check-runs(?:\?|\")'
watcher_reads_checks = bool(re.search(commit_checks, watcher))
assert re.search(commit_checks, pin_tracker), "Expected control: the pin tracker reads check runs"
print("Base watcher reads Actions runs:", '/actions/runs?' in watcher)
print("Base watcher reads commit statuses:", '/status?' in watcher)
print("Base watcher reads check runs by commit:", watcher_reads_checks)
print(f"Fixture: {actions['total_count']} workflow runs, {statuses['total_count']} statuses, "
      f"{checks['total_count']} check run with conclusion {checks['check_runs'][0]['conclusion']}")
assert watcher_reads_checks, "The sole failed check has no base CI input or fix-forward thread"
