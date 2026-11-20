"""Source-level reproduction of #482 replay with two distinct reverted lands."""

import json
from pathlib import Path
import re
import sqlite3


root = Path(__file__).resolve().parent.parent
events_source = (root / "crates/velocity-rosen/src/events.rs").read_text(encoding="utf-8")
store_source = (root / "crates/velocity-store/src/rosen.rs").read_text(encoding="utf-8")
checker_source = (root / "crates/velocity-core/src/rosen_checker.rs").read_text(encoding="utf-8")

change = "mnopqrstuvwxyzmnoqrstuvwxyzmnop"
report = "check exited with code 1"
events = [
    (1, "snapshot", {"named": True, "commit": "first head"}),
    (2, "landed", {"commit": "first land"}),
    (3, "reverted", {"culprit": "first land", "output": report}),
    (4, "snapshot", {"named": True, "commit": "second head"}),
    (5, "landed", {"commit": "second land"}),
    (6, "reverted", {"culprit": "second land", "output": report}),
]
with sqlite3.connect(":memory:") as database:
    database.execute("CREATE TABLE events (seq INTEGER, kind TEXT, change_id TEXT, detail TEXT)")
    database.executemany(
        "INSERT INTO events VALUES (?, ?, ?, ?)",
        [(position, kind, change, json.dumps(detail)) for position, kind, detail in events],
    )
    query = re.search(r'"(SELECT change_id, kind, MAX\(seq\).*?GROUP BY change_id, kind)"', events_source, re.S)
    if query is None:
        raise RuntimeError("Could not locate the landed replay query")
    maxima = {kind: position for _, kind, position in database.execute(query[1], (6,))}

latest_terminal = max(position for kind, position in maxima.items() if kind != "snapshot")
if "event.seq < latest.terminal" not in events_source or latest_terminal != 6:
    raise RuntimeError("Could not verify the replay cutoff for the second land and revert")
print(f"Replay boundary 6 supersedes the second land at 5 and delivers the revert at {latest_terminal}.")

# State was last saved in Review after naming the repair at 4. Its transcript
# contains the first revert notice from 3. The app missed both events 5 and 6.
reverted_message = checker_source.split("pub fn reverted_message(", 1)[1]
message_format = re.search(r'"(Your Rosen change `[^"\n]*(?:\\"[^"\n]*)*?\\n)"', reverted_message)
if message_format is None:
    raise RuntimeError("Could not locate the landed revert prompt format")
template = json.loads(f'"{message_format[1]}"')
first = events[2][2]
second = events[5][2]
assert first["culprit"] != second["culprit"]
old_message = template.format(change[:8], first["output"])
new_message = template.format(change[:8], second["output"])

message_only_guard = re.search(
    r"RosenProgress::Reverted if replaying =>\s*\{\s*"
    r"thread\.state != ThreadState::Merged && notice_handled\(thread, message\)\s*\}",
    store_source,
)
transcript_check = "TranscriptBlock::UserMessage { text, .. } if text == message"
skipped = message_only_guard is not None and transcript_check in store_source and new_message == old_message
print(f"Distinct culprits {first['culprit']!r} and {second['culprit']!r} produce identical notices: {new_message == old_message}.")
assert not skipped, "The second revert is skipped in Review, so Velocity never queues its Reopen follow-up."
