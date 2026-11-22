"""Check that Rosen-backed AIRC refusals fall back to the holder's change (#679).

Adapted from Scout's reproduction in mESmaC/velocity#679 (`target/scout-532-airc.py`
with `script_arguments: ["refused-change"]`): the original script bundled three
separate checks behind `sys.argv[1]`, while the stress runner invokes Python scripts
without arguments. This script runs the `refused-change` check by default and keeps
its original assertion alongside checks that remote refusal projection, cache lookup,
and project routing all handle the holder fallback while retaining requester
attribution when available.
"""

from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parent.parent
CHANGE = "zrqrpzzruukpynwnvyporzrwpxplrvqq"


def source(path):
    return (ROOT / path).read_text(encoding="utf-8")


def between(text, start, end):
    return text.split(start, 1)[1].split(end, 1)[0]


def refused_change():
    path = "crates/velocity-airc/src/remote_events.rs"
    rust = source(path)
    projection = between(rust, "ChannelEvent::Refused { nick, holder }", "ChannelEvent::Released")
    cache = between(rust, "fn attach_change(", "fn is_change_related(")
    routing = between(rust, "async fn route_project_entries(", "fn route_overlap(")
    # A joined coordinator has no checkout or owned change. Only the holder does.
    holder = {"nick": "claim-owner-50", "member": "local/claim-owner-50", "change": CHANGE}
    record = {"member": "velocity", "nick": "coordinator"}
    nick_changes = {holder["nick"]: holder["change"]}
    if ("holder.nick" in projection and "holder.member" not in projection
            and "record.change" not in projection and "record.claim" not in projection
            and "self.nick_changes.get(&record.nick)" in cache
            and "owner_changes.get(&record.nick)" in routing
            and nick_changes.get(record["nick"]) is None):
        print(f"FAIL: remote refused row cannot resolve holder {holder['nick']}'s change {CHANGE}.")
        print("The projection discards the holder identity after rendering text, and both fallbacks use the requester's nick.")
        return 1
    if "record.claim = Some(claim(&holder" not in projection:
        print("FAIL: ChannelEvent::Refused projection does not preserve the holder claim.")
        return 1
    if 'record.kind == "refused"' not in cache:
        print("FAIL: RemoteEventCache::attach_change does not separate requester and holder fallback for refused entries.")
        return 1
    resolve_routing = between(rust, "fn resolve_record_change(", "async fn route_project_entries(")
    if 'record.kind == "refused"' not in resolve_routing:
        print("FAIL: ProjectRouting::resolve_record_change does not handle requester and holder fallback for refused entries.")
        return 1
    return 0


if __name__ == "__main__":
    selected = sys.argv[1] if len(sys.argv) > 1 else "refused-change"
    if selected != "refused-change":
        print(f"Unknown check: {selected}")
        raise SystemExit(2)
    raise SystemExit(refused_change())
