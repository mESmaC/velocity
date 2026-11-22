"""Trace a per-issue overlap comment error to the whole F9 dispatch pass."""
from pathlib import Path
import re
import sys


overlap = Path("src-tauri/src/continuous_dispatch/overlap.rs").read_text(encoding="utf-8")
dispatch = Path("src-tauri/src/continuous_dispatch.rs").read_text(encoding="utf-8")
comments = Path("crates/velocity-github/src/rosen_issue.rs").read_text(encoding="utf-8")
hold = overlap.split("fn hold(", 1)[1].split("    fn ", 1)[0]
update = overlap.split("fn update(", 1)[1].split("    fn ", 1)[0]
recheck = overlap.split("fn recheck(", 1)[1].split("    fn ", 1)[0]
refresh = overlap.split("fn refresh(", 1)[1].split("\n}\n", 1)[0]
comment_lookup = comments.split("fn overlap_review_returned(", 1)[1].split("    fn ", 1)[0]

propagates = (
    "self.issue_comment_bodies(number)?" in comment_lookup
    and re.search(r"\.overlap_review_returned\(number\)\s*\.map_err\([^\n]+\)\?", hold)
    and "self.hold(number, &paths, blockers, waits, released)?" in update
    and re.search(r"self\.update\([^;]+\)\?;", recheck)
    and re.search(r"\.recheck\([^;]+\)\?;", refresh)
    and re.search(r"overlap::refresh\(application, project\.id, reader\)\?;\s*"
                  r"parked::refresh\(application, project, reader\)\?;\s*"
                  r"start_ready\(application, project\.id, Some\(reader\)\)", dispatch)
)
if propagates:
    print("FAIL: when the review-comment request for issue #126 fails, the overlap pass "
          "returns before start_ready. With one free slot, unrelated ready issue #133 "
          "cannot start even though its backlog and comments are available.", file=sys.stderr)
    sys.exit(1)
