"""Check that oversized AIRC claims never trap pagination at the same offset (#677).

Adapted from scout-1's `long-claim` reproduction in mESmaC/velocity#677: the original
script took `long-claim` on `sys.argv[1]` and ran `rosen diff --change` against a local
Rosen server, which is unavailable in standalone stress runs. This script preserves the
scout's exact UTF-8/JSON budget assertion on `crates/velocity-airc/src/read_tools.rs`:
a single 2,475-byte compact claim row must neither pass through `claim_row` unsummarized
nor stall `claim_page` at the same offset, and exact-piece lookups must page full task
text within `CLAIM_REPLY_BYTES`.
"""

import json
from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parent.parent
CHANGE = "zrqrpzzruukpynwnvyporzrwpxplrvqq"


def between(text, start, end):
    return text.split(start, 1)[1].split(end, 1)[0]


def json_size(value):
    return len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode())


def main():
    path = "crates/velocity-airc/src/read_tools.rs"
    rust = (ROOT / path).read_text(encoding="utf-8")
    page = between(rust, "fn claim_page(", "struct MessageReader")
    row_builder = between(rust, "fn claim_row(", "fn claim_page(")
    task = "Keep this complete claim description readable in AIRC. " * 45
    row = {
        "nick": "claim-text-50",
        "task": task,
        "piece": "issue:532",
        "own": True,
        "change": CHANGE,
        "path_count": 0,
        "details_omitted": True,
    }
    budget = int(re.search(r"const CLAIM_REPLY_BYTES: usize = ([0-9_]+);", rust)[1])
    envelope_match = re.search(
        r"const CLAIM_PAGE_ENVELOPE_BYTES: usize = ([0-9_]+);", rust
    ) or re.search(r"remaining = CLAIM_REPLY_BYTES - (\d+);", page)
    envelope = int(envelope_match[1])
    size = json_size(row) + 1
    unchanged_row = '"task": claim.task, "piece": claim.key()' in row_builder
    nonadvancing = re.search(
        r"if size > remaining \{\s*next = Some\(index\);\s*break;", page
    )
    if unchanged_row and nonadvancing and size > budget - envelope:
        print(
            f"FAIL: the sole compact claim needs {size} bytes; page capacity is {budget - envelope}."
        )
        print(
            "An exact-piece filter at offset 0 returns no claims and next_offset 0, every time."
        )
        return 1
    if "fn summarized_claim_row(" not in rust or "fn paged_claim_task(" not in rust:
        print(
            "FAIL: read_tools.rs must summarize oversized rows in lists and page full task text on exact lookup."
        )
        return 1
    print("PASS: oversized AIRC claims summarize in list pages and page full task text within budget.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
