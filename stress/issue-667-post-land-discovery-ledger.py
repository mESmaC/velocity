"""Check that automatic post-land discoveries use Rosen's native `discovery` ledger (#667).

Adapted from scout-1's reproduction in mESmaC/velocity#667: the original script
queried immutable historical event 17339 on a live Rosen pipe, which cannot pass
once that historical record exists and prints INCONCLUSIVE without a live server.
This script preserves what the scout asserted against the implementation:
`Channel::record_discovery` must append to Rosen's native `discovery` ledger
using the `velocity-airc/discovery/v1:` envelope unwrapped by Rosen's ledger
forwarder (never `Remote::append_record` / `past_work` / `velocity-airc/v1:`),
while preserving the change ID and deduplication behavior.
"""

from pathlib import Path
import re
import sys


root = Path(__file__).resolve().parent.parent
channel = (root / "crates/velocity-airc/src/channel.rs").read_text(encoding="utf-8")
remote = (root / "crates/velocity-airc/src/remote.rs").read_text(encoding="utf-8")
remote_events = (root / "crates/velocity-airc/src/remote_events.rs").read_text(encoding="utf-8")

record_discovery = channel.split("pub fn record_discovery(", 1)[1].split(
    "pub fn record_discovery_once(", 1
)[0]
if "remote.append_record(" in record_discovery:
    print(
        "FAIL: Channel::record_discovery routes to Remote::append_record, "
        "which writes a velocity-airc/v1: envelope to Rosen's 'past_work' ledger "
        "instead of the native 'discovery' ledger."
    )
    sys.exit(1)

if "remote.append_discovery(" not in record_discovery:
    print("FAIL: Channel::record_discovery does not call remote.append_discovery.")
    sys.exit(1)

append_discovery = remote.split("pub(crate) fn discovery_append_request(", 1)[1].split(
    "pub(crate) fn append_record(", 1
)[0]
targets_native_discovery = (
    '"ledger": "discovery"' in append_discovery
    and "DiscoveryEnvelope::encode_record(" in append_discovery
    and "RECORD_PREFIX" not in append_discovery
)
if not targets_native_discovery:
    print(
        "FAIL: Remote::discovery_append_request does not target Rosen's native "
        "'discovery' ledger with DiscoveryEnvelope."
    )
    sys.exit(1)

ledger_entry = remote_events.split("fn ledger_entry(", 1)[1].split(
    "fn normalize_envelope(", 1
)[0]
restores_change = bool(
    re.search(r"stored\.member\.is_none\(\)", ledger_entry)
    and "discovery.change" in ledger_entry
    and "discovery.nick" in ledger_entry
    and "discovery.member" in ledger_entry
)
if not restores_change:
    print(
        "FAIL: remote_events::ledger_entry does not restore change/nick/member "
        "from Velocity-authored discovery envelopes."
    )
    sys.exit(1)

print("PASS: automatic post-land discoveries target Rosen's native 'discovery' ledger.")
sys.exit(0)
