# Risk (#557, SPEC.md §3.1/F15): a repo-specific gh call can be attributed
# to the global bucket, hiding the affected project's rate.
# Structural regression probe of the CLI command's production counter path.
from pathlib import Path
import re
import sys

source = Path("crates/velocity-github/src")
repository = (source / "repository.rs").read_text(encoding="utf-8")
command = (source / "command.rs").read_text(encoding="utf-8")
counter = (source / "request_counter.rs").read_text(encoding="utf-8")
known_repository = re.search(
    r'gh\(&\["repo", "view", &repository\.full_name\(\)', repository
)
global_wrapper = re.search(
    r'fn gh\([^\n]+\{\s*crate::request_counter::record\(', command
)
global_counter = re.search(r'record_for_project\("\(global\)", caller, operation\)', counter)
if known_repository and global_wrapper and global_counter:
    print("repository_exists sends gh repo view owner/name through the unscoped CLI wrapper.")
    print("The counter receives (global) as its project even though owner/name is available.")
    sys.exit("A repo-scoped gh invocation must increment that repository's diagnostic row.")
