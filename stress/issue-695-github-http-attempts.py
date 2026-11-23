# Risk (#557, SPEC.md §3.1/F15): an HTTPS attempt that returns an error
# after dispatch can disappear from the per-project call rate and breach log.
# Structural regression probe of the transport's request and worker-error paths.
from pathlib import Path
import re
import sys

transport = Path("crates/velocity-github/src/http_transport.rs").read_text(encoding="utf-8")
counter = Path("crates/velocity-github/src/request_counter.rs").read_text(encoding="utf-8")
retry = transport.split("fn dispatch_with_auth_retry(", 1)[1].split("\nfn record_outcome", 1)[0]
worker = transport.split("async fn run_http_job(", 1)[1]
early_returns = re.findall(r"dispatch_once\([^;\n]+\)\?;\s*record_outcome\(", retry)
meter_before_response = "record_for_project(" in worker.split("request.send()", 1)[0]
if len(early_returns) == 2 and not meter_before_response:
    assert "request.send().await" in worker and "GitHub HTTP body read failed" in worker
    assert "record_gh_call(project," in counter
    print("Both the initial dispatch and auth retry return errors before the call counter.")
    print("The worker can return an error even after receiving GitHub's HTTP status.")
    sys.exit("Four such attempts in a steady-state minute must be counted for the project.")
