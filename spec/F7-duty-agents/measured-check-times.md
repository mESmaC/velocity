#### Measured check times (#387, 2026-11-14–15)

<!-- split-spec: {"order":939,"kind":"markdown","indentation":0} -->
**Measured check times (#387, 2026-11-14–15).**
The last 20 first-parent lands at `f87b0696` were replayed after warm-up parent `14cc9d01`, on a Windows Core Ultra 7 265F with Rust 1.99.0, Node 24.18.0, pnpm 10.34.9 and sccache 0.19.0.
Before uses the former pre-land commands: formatting, UI checks, locked workspace build, bindings export and workspace clippy.
Fast uses the final scripts with default incremental compilation.
All 20 pairs passed.
Seconds exclude run-queue waits and tree checkout.
The separate fast warm-up took 117.3 s and reported 72.8% machine cache hits (617 hits, 231 misses).

| Land (commit) | Before (s) | Fast (s) |
| --- | ---: | ---: |
| #362 (1f2d72af) | 77.9 | 53.9 |
| #363 (e5e520a6) | 59.2 | 62.1 |
| #364 (59be9087) | 64.7 | 37.0 |
| #365 (02677cca) | 122.0 | 43.2 † |
| #366 (8ee74749) | 6.8 ‡ | 48.2 |
| #367 (b3a2273e) | 82.0 | 54.2 |
| #368 (e282bdd2) | 5.1 | 0.7 |
| #370 (df63df22) | 125.3 | 29.0 † |
| #371 (c8446472) | 34.8 | 1.6 |
| #374 (6a42be41) | 11.4 | 1.1 |
| #340 (3f005afe) | 60.8 | 42.2 |
| #349 (1ab4f6ee) | 30.6 | 50.7 |
| #355 (2f64cf8f) | 27.2 | 11.4 |
| #372 (c819091e) | 71.7 | 87.7 |
| #344 (4b4c817f) | 33.5 | 21.9 |
| #293 (2be10da1) | 32.6 | 20.8 |
| #308 (3516b181) | 37.4 | 67.0 |
| #356 (8791d748) | 36.3 | 63.3 |
| #354 (75dd7422) | 29.7 | 67.6 |
| #347 (f87b0696) | 31.4 | 36.4 |

Across these 20 runs, mean time was **49.0 → 40.0 s**, median **35.6 → 42.7 s**, and maximum **125.3 → 87.7 s**; 15 fast runs finished under 60 s.
Some changes were slower.
† These two fast timings are retries after interrupted compilations and benefit from partially built artifacts; they understate first-attempt cost.
‡ This baseline already had compiled artifacts from an interrupted pilot.
Other completed baseline measurements were retained rather than rebuilt.
Concurrent builds, upgrades and changing thread budgets varied the cache and CPU conditions.
Both sets also encountered incremental-directory access-denied notes ([Rosen #381](https://github.com/mESmaC/rosen/issues/381)); run-client lifetime failures are tracked in [Rosen #382](https://github.com/mESmaC/rosen/issues/382).
The raw pair timings and these limits are retained so the aggregate is not treated as a uniform speedup.

A separate fresh function-body edit in `velocity-updates`, on the fully checked merge tree, completed in **10.2 s** (10.10 s in the feed).
It selected only `velocity-updates` and its reverse dependent `velocity-app`, with bindings and UI both skipped.
The edit was restored byte-for-byte afterward; this meets the warm one-crate target on this run.
