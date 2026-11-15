#### Warm pool (speed) · Lean · Built

<!-- split-spec: {"order":717,"kind":"markdown","indentation":2} -->
- Built: the prebuild worker does it for each Lake project (a `lean-toolchain` and a lakefile) at the checkout's root or in a top-level folder such as `proofs/`, before the Cargo build and whether or not the Rust build cache is shared.
  Mathlib's cache tool keeps its downloads in one machine-wide folder, so only the first worktree on a machine downloads Mathlib and later ones unpack it.
  Before Lake runs, each package pinned in `lake-manifest.json` that the worktree lacks is cloned locally from another checkout of the repository that has the pinned commit (the main checkout first), with the manifest's URL as `origin`, so Lake uses it instead of cloning Mathlib's history again.
  A failed step is not retried until `master` moves, like the Cargo prebuild.
