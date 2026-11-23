# Risk (#554, F1/F4): one streamed thread row can advance the project-wide
# revision past a pending update for a different row in the same project.
from pathlib import Path
import re


def source(file):
    return Path(file).read_text(encoding="utf-8")


core = source("crates/velocity-core/src/layout_state.rs")
threads = source("src-tauri/src/threads.rs")
projection = source("crates/velocity-store/src/project_layout.rs")
update = core.split("pub fn replace_thread_entry(", 1)[1].split("pub fn replace_data(", 1)[0]
rebuild = threads.split("fn rebuild_layout(", 1)[1].split("\nfn ", 1)[0]
single_row = re.search(
    r'\.find\(\|thread\| thread\.policy\.identifier == entry\.policy\.identifier\)', update,
)
skip = re.search(
    r'if self\.data\.project_identifier != Some\(project\)\s*\|\|\s*'
    r'self\.store_revision_is_stale\(revision\)\s*\{\s*return true;', update,
)
advance = re.search(r'\*existing = entry;\s*self\.store_revision = Some\(revision\);', update)
if (single_row and skip and advance
        and "Some((self.state.revision, entry, Some(project.spec_locked)))" in projection
        and rebuild.index(".layout_thread_update(") < rebuild.index("state::<MainLayout>()")
        and re.search(r'if applied \{.*?return Ok\(\(\)\);', rebuild, re.S)):
    print("1. Thread A saves new activity and captures its row at store revision 10.")
    print("2. Thread B captures its row at revision 11 and applies it first, touching only B.")
    print("3. A's update returns success as stale. The fallback never reloads the missing A row.")
    raise SystemExit("The sidebar can keep A's old status after both workers publish.")
