#### Rosen · Rebases at the step boundary (#271, #398) · A conflict (#379, #390, #398)

<!-- split-spec: {"order":605,"kind":"markdown","indentation":2} -->
- **A conflict (#379, #390, #398)** moves the change onto the new base with a rebased head that can contain conflict markers.
  Velocity saves the conflict event's sequence and notice together, then waits for a turn boundary and verifies that the workspace has taken the current head before sending the notice.
  A busy agent is never told to resolve an unapplied workspace.
  Failed application or delivery stays pending across reconnects and restarts, retried at turn end and every 30 seconds.
  Replayed events and notices already in the saved transcript are not sent again; a notice is discarded if the server's conflicts were resolved or the change closed meanwhile.
  The message identifies the conflict event and files, explains the applied markers and asks the agent to preserve both changes, run `rosen resolve` on each remaining conflicted file, then name the work again with `rosen commit -m`.
  Resolve snapshots the workspace first.
  Rosen takes resolved snapshots of already conflicted files directly without nesting markers, and refuses both resolution and landing while conflict markers remain, even if all conflict records were cleared (rosen#383).
  Generated artifacts belong in `rosen.toml`'s `generated` list, never in `registration_lists` (#404).
  A rebase's `regenerate` paths get their own durable notice, saved with the event cursor and delivered only after the rebased head reaches the workspace at a turn boundary, even when there are no text conflicts.
  A later text-conflict notice does not replace pending regeneration work; delivered notices are not repeated and a closed change gets no regeneration notice.
  The notice names `rosen run -- cargo metadata --format-version 1` for `Cargo.lock`, `pnpm -C ui install --lockfile-only` for `ui/pnpm-lock.yaml`, and `rosen run -- pnpm -C ui bindings` for `ui/src/lib/bindings.ts`, run from the project root before naming the work again.
  Other generated paths use their project's generator.
