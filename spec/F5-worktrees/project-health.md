#### Rosen · Project health checks (#509)

Velocity monitors each Rosen project in the background, performs safe automatic remediation when a signal crosses its threshold, and shows active signals on a project health strip with the action taken and when.

| Signal | Automatic action |
|---|---|
| Mirror behind the base for more than 10 min | Retry the push via Rosen catch-up; if diverged or refused, show the mirror notice with both tips, and apply #501 catch-up where the mirror is ahead. |
| Integration build has not covered the base for 10 lands or 2 hours | Start an integration build; if it cannot start (running, paused, low memory, another project holding the slot, or last build failed on the base), say why on the project. |
| Integration or GitHub CI red on the base | Start or track the fix-forward thread (#507). |
| Landed issue still open more than 6 hours with no green integration | Show "N landed issues wait for a green integration build" with the last build's outcome (#504). |
| Issue parked under `no-change` past its review limit | Resolve per the no-change review rules (#506) and surface automatic returns, failures, or manual holds. |
| Thread in review more than 20 min without landing | Ask Rosen for the land refusal (`land_check`), sync if already landed, retry if landable, or send the refusal to the author (#487). |
| Slots free and open issues present, but nothing dispatched for 10 min | Log why each open issue was skipped (overlap, label, blocker, owner decision, hold, or claim) to `velocity.log` and show the summary on the strip. |
| A post-land check stopped for running too long | Capture the active child process before stopping the tree, parse the running test from the log, and report both on the strip and in the failure report (#486). |

Each strip row shows its severity, signal, automatic action taken or block reason, timestamp, optional detail line, and a link to open an owning thread when one exists.
When every signal is clear, the strip stays hidden.
