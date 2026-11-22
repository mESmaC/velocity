"""Source-level check for #678: build members with reserved slug prefixes in AIRC.

Adapted from scout reproduction in mESmaC/velocity#678 (target/scout-532-airc.py reserved-slug):
defaults the mode argument to "reserved-slug" when invoked without CLI arguments by the stress
runner; the reserved_slug() assertion body is preserved unchanged.
"""

from pathlib import Path
import re
import sys


ROOT = Path(__file__).resolve().parent.parent


def source(path):
    return (ROOT / path).read_text(encoding="utf-8")


def between(text, start, end):
    return text.split(start, 1)[1].split(end, 1)[0]


def reserved_slug():
    path = "crates/velocity-core/src/thread_titles.rs"
    rust = source(path)
    generator = between(rust, "pub fn thread_agent_name(", "pub fn thread_id_from_member(")
    parser = between(rust, "pub fn thread_id_from_member(", "fn stable_hash(")
    dependencies = source("crates/velocity-threads/src/dependencies.rs")
    if 'format!("{slug}-{thread_id}")' not in generator:
        return 0
    workspace = "spec-cleanup"
    limit = int(re.search(r"AGENT_SLUG_LENGTH: usize = (\d+);", rust)[1])
    assert len(workspace) <= limit
    name = f"{workspace}-50"
    excluded = re.findall(r'member\.starts_with\("([^"]+)"\)', parser)
    if name.startswith(tuple(excluded)) and "thread_id_from_member(&waiting.holder).is_none()" in dependencies:
        print(f"FAIL: build thread 50 gets member {name}; the member parser returns None.")
        print("F14 dependency refresh skips this holder even after the saved thread fails or closes.")
        return 1
    return 0


if __name__ == "__main__":
    mode = sys.argv[1] if len(sys.argv) > 1 else "reserved-slug"
    checks = {"reserved-slug": reserved_slug}
    raise SystemExit(checks[mode]())
