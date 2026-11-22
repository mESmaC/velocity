"""Check an upgraded F9 dispatch pass with a repeat hold saved by the old build."""
from pathlib import Path
import sys


overlap = Path("src-tauri/src/continuous_dispatch/overlap.rs").read_text(encoding="utf-8")
comments = Path("crates/velocity-github/src/rosen_issue.rs").read_text(encoding="utf-8")
update = overlap.split("fn update(", 1)[1].split("    fn ", 1)[0]
hold = overlap.split("fn hold(", 1)[1].split("    fn ", 1)[0]

review_comment = (
    "The whole issue remains.\n"
    "<!-- velocity-no-change-review:overlap:109:886f5b26e42db8f00131142d74cbb027de288afb:false -->"
)
saved_wait = {"blocker": 106, "blocker_thread": 7, "paths": []}
waiting_issue_paths = {"web/routes/explore.tsx"}
current_blocker_edits = {"web/routes/explore.tsx"}

checks_new_holds = (
    ".overlap_review_returned(number)" in hold
    and 'strip_prefix("<!-- velocity-no-change-review:")' in comments
    and 'marker.ends_with(":false")' in comments
)
existing_wait_skips_review_record = (
    "if let Some(wait) = waits.get(&number)" in update
    and "overlap_review_returned" not in update
    and "wait.paths = shared;" in update
    and update.index("wait.paths = shared;") < update.index("self.hold(")
)
if checks_new_holds and existing_wait_skips_review_record:
    assert review_comment.splitlines()[-1].endswith(":false -->")
    assert saved_wait["blocker_thread"] == 7
    saved_wait["paths"] = sorted(waiting_issue_paths & current_blocker_edits)
    if saved_wait["paths"]:
        print("FAIL: the saved second hold behind #106 survives the first upgraded pass "
              "despite the earlier returned-to-dispatch review for #109.", file=sys.stderr)
        sys.exit(1)
