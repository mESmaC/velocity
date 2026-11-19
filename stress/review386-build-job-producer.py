# Risk (#386, SPEC.md F5): issue closure consumes Rosen's durable Build prefix,
# but no application runner executes/submits the offered Build job, so a normal
# successful post-land test run leaves every dispatched issue open forever.
# This structural probe traces the production request sites; the pinned protocol
# only advances its Build prefix after PostLandPassed of a Build job.
# #424 moved the production verification runner under landing_runner.
from pathlib import Path
import re

root = Path(__file__).resolve().parent.parent
reader = root / "crates/velocity-rosen/src/issue.rs"
adapter = root / "crates/velocity-rosen/src/post_land.rs"
runner = root / "crates/velocity-threads/src/landing_runner/post_land.rs"

print("Post-land runner calls next_batch:", "post_land::next_batch(" in runner.read_text(encoding="utf-8"))
print("next_batch requests test work:", ".post_land_next()" in adapter.read_text(encoding="utf-8"))
sites = []
for directory in [root / "crates", root / "src-tauri/src"]:
    for path in directory.rglob("*.rs"):
        text = path.read_text(encoding="utf-8")
        if re.search(r"\.post_land_build_next\s*\(|Request::PostLandBuildNext", text):
            sites.append(path)
print("Build work request sites:", [str(path.relative_to(root)) for path in sites])
producers = [path for path in sites if path != reader]
print("Build job runners outside the last_green-only issue reader:", len(producers))
assert producers, (
    "No producer advances the durable Build prefix consumed by issue_build_coverage; "
    "the normal successful land/test path cannot close an issue"
)
