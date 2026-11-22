from pathlib import Path
import re
import tempfile
import tomllib
import sys

source = Path("crates/velocity-rosen/src/project.rs").read_text(encoding="utf-8")
writer = source.split("fn write_defaults(", 1)[1].split("fn seed_new(", 1)[0]
bootstrap = source.split("async fn import_initial_base(", 1)[1].split("fn write_defaults(", 1)[0]
planner = Path("crates/velocity-core/src/new_project.rs").read_text(encoding="utf-8")
mirror = "https://api.collapse.sh/example/project.git"
root = mirror.removesuffix(".git")
with tempfile.TemporaryDirectory() as directory:
    config = Path(directory) / "rosen.toml"
    config.write_text('review = "after-land"\n', encoding="utf-8")
    admits_folder = "NewProjectLocation::NewFolder => Ok(false)" in planner
    imports_after_write = re.search(r"write_defaults\(root, mirror, collapse\.as_deref\(\)\)\?;.*?seed_new\(root,", bootstrap, re.S)
    returns_early = re.search(r"if path\.exists\(\) \{\s*return Ok\(\(\)\);\s*\}", writer)
    if not (admits_folder and imports_after_write and returns_early and config.exists()):
        sys.exit(0)
    landed = tomllib.loads(config.read_text(encoding="utf-8"))
    bad_create = (landed.get("collapse"), landed.get("mirror", {}).get("remote")) != (root, mirror)
    if bad_create:
        print("FAIL Create source path: admitted non-Git folder; write_defaults returns on existing rosen.toml before seed_new; fixture has neither collapse nor [mirror] remote.")

sys.exit(1 if bad_create else 0)
