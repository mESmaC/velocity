# F5 Rosen Base CI (#507): the fix-forward brief must identify the failed job.
# This source-level probe supplies a second-page matrix failure to the job list
# endpoint used by the watcher. GitHub allows 256 matrix jobs and 100 per page.
from pathlib import Path
import re

client = Path("crates/velocity-github/src/workflow_runs.rs").read_text(encoding="utf-8")
body = client.split("pub fn jobs(&self, run: u64)", 1)[1].split("\n    }", 1)[0]
one_page = bool(re.search(r'"/actions/runs/\{run\}/jobs\?per_page=100"', body)
                and re.search(r"Ok\(self\.read::<Jobs>\(&path\)\?\.jobs\)", body)
                and not re.search(r"&page=|paginate|total_count", body))

jobs = [{"name": f"test (shard {number})", "conclusion": "success"}
        for number in range(1, 101)]
jobs.append({"name": "test (macOS, shard 101)", "conclusion": "failure",
             "steps": [{"name": "Run tests", "conclusion": "failure"}]})
first_page = {"total_count": len(jobs), "jobs": jobs[:100]}
second_page = {"total_count": len(jobs), "jobs": jobs[100:]}
print("WorkflowClient.jobs returns one page of at most 100:", one_page)
print("Page 1 total_count:", first_page["total_count"])
print("Page 2:", second_page["jobs"])
if one_page:
    observed = [job for job in first_page["jobs"] if job["conclusion"] == "failure"]
    print("Failed jobs reaching the repair planner:", observed)
    assert observed, "The only failed matrix job is on page 2"
