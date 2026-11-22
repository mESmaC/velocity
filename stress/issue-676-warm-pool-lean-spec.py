"""Check the F5 warm-pool preparation contract against its Lean rule."""
from pathlib import Path
import sys
import tomllib

prepare = tomllib.loads(Path("rosen.toml").read_text(encoding="utf-8"))["prepare"]["commands"]
rules = Path("spec/F5-worktrees")
queue = (rules / "warm-pool-build-queue.md").read_text(encoding="utf-8")
lean = (rules / "warm-pool-lean-built.md").read_text(encoding="utf-8")
expected = next(line for line in lean.splitlines() if "templates skip Lean setup" in line)
stale = "Warm pool prepares install UI dependencies and fetch the Lean cache."
print(f"Rosen [prepare] commands: {prepare}")
print(f"F5 Lean Built: {expected.strip()}")
if stale in queue and not any("lake exe cache get" in command for command in prepare):
    print(f"FAIL: F5 Build queue still says: {stale}")
    sys.exit(1)
