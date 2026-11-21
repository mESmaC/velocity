<!-- split-spec: {"order":1253,"kind":"markdown","indentation":0} -->
### F12 · Thread context menu and delete

Right-clicking a thread in the sidebar opens a menu:
- Open workspace in Explorer
- Copy workspace path
- **Delete …** (shown in red)

The delete label depends on the thread:
- **Delete thread and workspace**, with what it will do underneath: "stops the agent · 247 MB freed"
- **Delete swarm and its N workspaces** (swarm row)
- **Delete thread** (merged threads, whose workspace is already gone, and spec threads)

Duty threads have no delete entry.

Deleting a swarm or goal swarm row deletes every member's thread as below, under one toast and one Undo, then removes the swarm and closes its view.

Delete is **immediate, with no confirmation** (decided).
1.
   The thread disappears and a toast offers **Undo** for 5 s.
2.
   When the 5 s are up:
   - the agent is stopped;
   - the workspace is deleted;
   - any named change is closed;
   - a linked task goes back to todo, or a linked issue becomes free again.

   The thread is gone either way.
   Deleting a merged thread whose Rosen land still has pending issue follow-ups (land comment, close on green, or reopen on revert) retains those follow-ups on the project until they complete.
   A workspace that can't be removed yet (the agent is still exiting or a file is still open) is retried in the background until it goes, and one that F5's verification keeps stays on disk; the toast says which.

**Accept when:**
- [ ] Undo within 5 s restores everything exactly.
- [ ] After 5 s nothing remains on disk.
