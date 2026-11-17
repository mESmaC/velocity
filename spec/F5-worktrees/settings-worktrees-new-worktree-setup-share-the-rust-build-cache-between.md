#### Settings → Workspaces · New workspace setup · "share the Rust build cache between a project's workspaces" (default on)

<!-- split-spec: {"order":661,"kind":"markdown","indentation":2} -->
- "share the Rust build cache between a project's workspaces" (default on): agents, setup commands and the pool's prebuilds get `RUSTC_WRAPPER=sccache` when sccache is installed and no wrapper is set, and each workspace builds in its own `target` folder

The agent starts only after setup finishes.
