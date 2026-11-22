from pathlib import Path
import re
import sqlite3
import sys
root = "https://api.collapse.sh/example/project"
impl = Path("crates/velocity-rosen/src/status/metadata.rs").read_text(encoding="utf-8")
pane = Path("ui/src/lib/RosenStatusPane.svelte").read_text(encoding="utf-8")
_, _, feed = impl.partition("fn state_feed(")
feed = feed.split("fn changes(", 1)[0]
sql = re.findall(r'"(SELECT [^"]+)"', feed)
if len(sql) < 2 or "events WHERE seq" not in sql[1]:
    sys.exit(0)
cursor_query, timestamp_query = sql[:2]
database = sqlite3.connect(":memory:")
database.executescript("""
CREATE TABLE events (seq INTEGER PRIMARY KEY, created_at INTEGER NOT NULL);
CREATE TABLE state_push (endpoint TEXT PRIMARY KEY, delivered INTEGER NOT NULL);
""")
endpoint = root + "/rosen/state"
database.execute("INSERT INTO events VALUES (42, 1700000000000)")
database.execute("INSERT INTO state_push VALUES (?, 42)", [endpoint])
delivered, = database.execute(cursor_query, [endpoint]).fetchone()
displayed_ms, = database.execute(timestamp_query, [delivered]).fetchone()
acknowledged_ms = 1700007200000
bad_time = "State feed last sent" in pane and displayed_ms != acknowledged_ms
if bad_time:
    print(f"FAIL actual status SQL: batch accepted at {acknowledged_ms} after a two-hour outage; pane labels {displayed_ms} as last sent (event timestamp).")
sys.exit(1 if bad_time else 0)
