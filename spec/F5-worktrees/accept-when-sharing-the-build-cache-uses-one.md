#### Accept when · Sharing the build cache uses one compiler cache shared by every worktree (sccache as RUSTC_WRAPPER for Rust) and a separate target folder per worktree (proposed mechanism)

<!-- split-spec: {"order":783,"kind":"markdown","indentation":0} -->
- [ ] Sharing the build cache uses one compiler cache shared by every worktree (sccache as `RUSTC_WRAPPER` for Rust) and a separate target folder per worktree *(proposed mechanism)*.
      A shared `CARGO_TARGET_DIR` is not used: cargo locks it, so agents would build one at a time.
