# Risk (#557, SPEC.md §3.1/F15): background saver lock wait has no caller row.
# Structural regression probe of the desktop saver and the store lock it acquires.
from pathlib import Path
import re
import sys

app = Path("src-tauri/src/state.rs").read_text(encoding="utf-8")
save = Path("crates/velocity-store/src/save_behind.rs").read_text(encoding="utf-8")
meter = Path("crates/velocity-store/src/store_lock.rs").read_text(encoding="utf-8")

raw_call = re.search(r"save_pending\(\s*[^\n]*\.as_mutex\(\)", app)
raw_lock = re.search(r"pub fn save_pending\(store: &Mutex<StateStore>\)", save)
if raw_call and raw_lock and "record_store_lock(" not in save:
    assert "record_save(" in save and "record_store_lock(" in meter
    print("save_now passes the underlying Mutex<StateStore> to save_pending.")
    print("save_pending starts measuring holds after lock(), and records only save aggregates.")
    sys.exit("A saver waiting for the store mutex must produce an attributed lock-wait sample.")
