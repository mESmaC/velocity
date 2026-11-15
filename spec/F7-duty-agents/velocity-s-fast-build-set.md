#### Velocity's fast build set (#387)

<!-- split-spec: {"order":919,"kind":"markdown","indentation":0} -->
**Velocity's fast build set (#387).**
`rosen.toml` keeps the fast commands in `checks.pre_land` and the full commands in `checks.post_land`; in after-land mode the build checker runs the former after each land (see below).
`node scripts/check-fast.mjs` runs locked `cargo check` and clippy on changed crates and their transitive reverse dependencies, including optional, build and target-specific dependencies.
Each check folder compares its inputs with its last successful run, so batching, deleted files and a bisection going backwards are covered.
A missing or incompatible record, a workspace manifest, lockfile, toolchain, Cargo configuration or checker-script change checks all crates.
Embedded files count against their owning crate.
Bindings use a conservative fingerprint of Rust declarations, command signatures, registration code, embedded interface inputs, generated bindings, manifests and the toolchain.
Ordinary function-body changes do not export bindings again; macros, manual schema computation, custom build scripts and unrecognised syntax retain broader inputs.
UI install, build, type check and lint run only when UI inputs (including bindings) change, or the folder needs its initial UI build.
The previous checkpoint is removed before the selected checks execute; only a complete pass writes its replacement.
The full set also checks formatting, links the workspace, checks crates independently and runs all workspace tests.

Every folder keeps its own Cargo target and Cargo's default incremental compilation for workspace crates; agents and check folders share sccache for cacheable registry and git dependencies.
Installed sccache 0.19 retains the working directory in Rust keys, so cross-folder workspace-crate hits are not assumed.
The build feed includes the fast script's elapsed time and cache hit/miss delta, labelled as a machine-wide delta because other agents can share the cache.
No cacheable requests is reported explicitly, as is an unavailable cache; counters are never reset.
Target: a warm fast run for one changed crate under 60 seconds, excluding its run-queue wait.
`node scripts/measure-checks.mjs` replays the latest 20 first-parent landed trees in order, with a separate warm-up parent, fixed old commands and separate persistent before/after folders.
The replay takes one `rosen run` turn, records its queue wait separately, and saves each sample's execution time, commit, cache report and log under `target/`.
