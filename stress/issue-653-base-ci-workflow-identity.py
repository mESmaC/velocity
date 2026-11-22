# F5 Rosen Base CI (#507): only a green run of the same workflow clears a failure.
# This source-level probe applies the existing name-first key and newest-run
# rule to two GitHub workflow records. The files and workflow IDs are distinct.
from pathlib import Path
import re

transport = Path("crates/velocity-github/src/base_ci.rs").read_text(encoding="utf-8")
core = Path("crates/velocity-core/src/base_ci.rs").read_text(encoding="utf-8")
conversion = transport.split("fn base_run(run: WorkflowRun)", 1)[1].split("\n}\n", 1)[0]
name_first = re.search(r"let workflow = run\s*\.name\s*\.filter\([^\n]+\)\s*\.or\(run\.path\)", conversion)
by_name = ".entry(run.workflow.as_str())" in core

failed = {"id": 101, "workflow_id": 11, "name": "CI", "path": ".github/workflows/macos.yml", "conclusion": "failure"}
passed = {"id": 102, "workflow_id": 12, "name": "CI", "path": ".github/workflows/docs.yml", "conclusion": "success"}
print("Base watcher uses workflow display name before file path:", bool(name_first))
print("Decision table groups on the resulting workflow field:", by_name)
print(f"Failed workflow: {failed['workflow_id']} {failed['path']}")
print(f"Passed workflow: {passed['workflow_id']} {passed['path']}")
if name_first and by_name:
    deciding = {}
    for run in sorted((failed, passed), key=lambda run: run["id"]):
        deciding[run["name"] or run["path"]] = run
    print("Deciding workflow records:", list(deciding.values()))
    assert failed in deciding.values(), "The green docs workflow hid the red macOS workflow"
