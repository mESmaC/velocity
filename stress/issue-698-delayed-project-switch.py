# Risk (#554, F1): a delayed project switch can restore an older projection
# after a Rosen event has published a more recent state for that same project.
from pathlib import Path
import re


def source(file):
    return Path(file).read_text(encoding="utf-8")


projects = source("src-tauri/src/projects.rs")
core = source("crates/velocity-core/src/layout_state.rs")
events = source("src-tauri/src/rosen_events.rs")
layout = source("src-tauri/src/layout.rs")
switch = projects.split("pub async fn switch_project(", 1)[1].split("pub fn publish_selection(", 1)[0]
restore = core.split("pub fn restore_store_project(", 1)[1].split("pub fn replace_thread_entry(", 1)[0]
apply = core.split("pub fn apply_store_data(", 1)[1].split("pub fn restore_store_project(", 1)[0]
captured_before_layout = re.search(
    r'let (?:mut )?switched = \{[^{}]*ManagedStateStore[^{}]*\.select_project\([^{}]*\};'
    r'[^{}]*let snapshot = \{[^{}]*MainLayout[^{}]*\.restore_store_project\(\s*'
    r'switched\.refresh\.revision,\s*switched\.refresh\.data,', switch, re.S,
)
unconditional_restore = re.search(
    r'\)\s*\{\s*self\.store_revision = Some\(revision\);\s*self\.restore_project\(', restore,
)
if (captured_before_layout and unconditional_restore
        and "self.store_revision_is_stale(revision)" in apply
        and "self.store_revision = Some(revision);" in apply
        and "apply_refresh_and_publish(application, refresh)" in events
        and "refresh.apply(&mut layout)" in layout):
    print("1. switch_project captures project B at store revision 10 and releases the store lock.")
    print("2. A Rosen event applies the full project B projection at revision 11.")
    print("3. The delayed switch restores revision 10 unconditionally and snapshots it for IPC.")
    raise SystemExit("Project B's newly published Work and thread state can regress after the switch.")
