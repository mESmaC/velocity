# Risk (#557, SPEC.md §3.1/F15): token usage can exceed the UI command budget
# while the latency row measures only the final reporting step.
# Structural regression probe of the registered command and shared sampler.
from pathlib import Path
import re
import sys

source = Path("src-tauri/src")
handler = (source / "token_usage.rs").read_text(encoding="utf-8")
layout = (source / "layout.rs").read_text(encoding="utf-8")
registry = (source / "lib.rs").read_text(encoding="utf-8")

if "token_usage::get_token_usage" not in registry:
    sys.exit("Review setup error: token-usage IPC registration moved")
command = handler.split("pub async fn get_token_usage(", 1)[1].split("\nasync fn trial_items", 1)[0]
coverage = re.search(r"trial_items\(&data, samples\)\.await;\s*"
                     r"crate::layout::(?:run_concurrently|off_ui_thread(?:_named)?)\(", command)
if coverage and "record_ui_command(" not in command and "record_ui_command(" in layout:
    print("get_token_usage awaits Rosen build coverage before entering the timed worker.")
    print("A 250 ms preflight plus a 10 ms report gives a 260 ms command and a 10 ms sample.")
    sys.exit("UI latency budgets must include the awaited work before the report.")
