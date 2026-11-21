#### Warm pool (speed) · Lean · Built

<!-- split-spec: {"order":717,"kind":"markdown","indentation":2} -->
- Built: warm-pool slots and Rosen `[prepare]` templates skip Lean setup so workspaces that never touch `proofs/` have no `proofs/.lake` (#546).
  When a workspace runs `lake` (through `queue_run`, the agent shell, `scripts/check.ps1`, `scripts/release-gate.ps1` or `velocity-proofs`), Velocity checks the Lake project's `lake-manifest.json` and runs `lake exe cache get` first whenever Mathlib's prebuilt cache is missing or its pinned revision changed, stamping `.lake/.velocity-mathlib-rev` so repeat builds skip the fetch.
  Mathlib's cache tool keeps its downloads in one machine-wide folder (`~/.cache/mathlib`), so only the first proof build on a machine downloads Mathlib and later workspaces unpack it on demand.
