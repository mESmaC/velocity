"""F5 Rosen Base CI (#558): compare a repair's land to its own failing job."""
from pathlib import Path
from typing import NamedTuple
import re


def function(path, name):
    source = Path(path).read_text(encoding="utf-8")
    start = re.search(rf"(?m)^.*\bfn {name}\(", source).start()
    return source[start:].split("\n}", 1)[0]


class Repair(NamedTuple):
    workflow: str
    thread: int
    covered: int
    land: int | None
    state: str


# Integer commit IDs stand for four commits on a single ancestry chain.
# The two workflows started repairs in sequence. Docs' old repair errored
# and is kept open for its unchanged failure. Build's repair landed at c3,
# but Build's latest failed run is still on c2. Docs failed later at c4.
failures = {"Build": 2, "Docs": 4}
repairs = [
    Repair("Docs", 11, 1, None, "error"),
    Repair("Build", 12, 2, 3, "landed"),
]

path = "src-tauri/src/project_health.rs"
check = function(path, "check_base_red")
select = function(path, "selected_ci_repair")
core = "crates/velocity-core/src/base_ci.rs"
action = function(core, "format_landed_repair_action")
poller = Path("src-tauri/src/base_ci.rs").read_text(encoding="utf-8")

# Source-level probe of the wiring, with the corresponding selection and
# ancestry rules applied to the fixture. It does not invoke Tauri or GitHub.
global_comparison = (
    "selected_ci_repair(application, project, server, ci.commit.as_deref())" in check
    and "resolve_commit_order(application, Some(server), landed, failing)" in select
    and ".or_else(|| project.ci_repairs.last())" in select
    and select.find("let repair =") < select.find("let commit_order =")
    and "new fix-forward queued" in action
)
poller_is_scoped = (
    ".find(|failure| failure.workflow == repair.workflow)" in poller
    or "matching_repair_failure(&repair, failures)" in poller
)
if global_comparison and poller_is_scoped:
    selected = repairs[-1]  # Both repairs are terminal; use the last record.
    latest_failure = max(failures.values())
    matching_failure = failures[selected.workflow]
    print(f"Health links thread #{selected.thread} ({selected.workflow}), landed c{selected.land}.")
    print(f"Health compares against Docs' red c{latest_failure} and says 'new fix-forward queued'.")
    print(f"The poller compares against Build's red c{matching_failure} and waits for CI.")
    assert (latest_failure >= selected.land) == (matching_failure >= selected.land), (
        "The health notification claims a replacement is queued before the repair's own workflow tests its land"
    )
