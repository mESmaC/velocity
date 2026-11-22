#### Accept when

<!-- split-spec: {"order":771,"kind":"markdown","indentation":0} -->
**Accept when:**

<!-- split-spec links -->

Rule files:

- [With a ready workspace in the pool, an agent starts working within 5 seconds of being launched](accept-when-with-a-ready-worktree-in-the.md)
- [Two agents never share a workspace](accept-when-two-agents-never-share-a-worktree.md)
- [Every land deletes its workspace within one event cycle](accept-when-every-merge-deletes-its-worktree-within.md)
- [Abandoned and failed threads keep their workspace until the thread is deleted](accept-when-closed-and-failed-threads-keep-their.md)
- [Changing the location never moves or breaks existing workspaces](accept-when-changing-the-location-never-moves-or.md)
- [Sharing the build cache uses one compiler cache shared by every workspace (sccache as RUSTC_WRAPPER for Rust) and a separate target folder per workspace (proposed mechanism)](accept-when-sharing-the-build-cache-uses-one.md)
- [Two agents' heavy builds never run at the same time, a landing check never waits behind a lower-priority build, and the footer shows the running build and the queue (proposed)](accept-when-two-agents-heavy-builds-never-run.md)
