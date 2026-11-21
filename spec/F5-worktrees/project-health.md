#### Rosen · Project health checks (#509)

Velocity monitors each Rosen project in the background, performs safe automatic remediation when a signal crosses its threshold, and shows active signals as project health notifications with the action taken and when.

| Signal | Automatic action |
|---|---|
| Mirror behind the base for more than 10 min | Retry the push via Rosen catch-up; if diverged or refused, show the mirror notice with both tips, and apply #501 catch-up where the mirror is ahead. |
| Collapse mirror reads need proactive auth | Check that Git 2.46 or later is installed and `http.https://api.collapse.sh/.proactiveAuth` resolves to `basic`; set `http.https://api.collapse.sh/.proactiveAuth` to `basic` in global git config when Git supports it, or report why proactive auth cannot be configured (#571). |
| Landed changes with no post-land result for more than 1 hour | Report "Landed N changes with no post-land result for > 1 h" with the oldest unverified commit, and start or explain why verification cannot run (#567). |
| Integration build has not covered the base for 10 lands or 2 hours | Start an integration build; if it cannot start (running, paused, low memory, another project holding the slot, or last build failed on the base), say why on the project. |
| Integration or GitHub CI red on the base | Start or track the fix-forward thread (#507); for GitHub CI, report which commit the repair covers and whether the latest failure is at or newer than its land (#558). |
| Landed issue still open more than 6 hours with no green integration | Show "N landed issues wait for a green integration build" with the last build's outcome (#504). |
| Issue parked under `no-change` past its review limit | Resolve per the no-change review rules (#506) and surface automatic returns, failures, or manual holds. |
| Thread in review more than 20 min without landing | Ask Rosen for the land refusal (`land_check`), sync if already landed, retry if landable, or send the refusal to the author (#487). |
| Slots free and open issues present, but nothing dispatched for 10 min | Log why each open issue was skipped (overlap, label, blocker, owner decision, hold, or claim) to `velocity.log` and show the summary in the notifications. |
| A post-land check stopped for running too long | Capture the active child process before stopping the tree, parse the running test from the log, and report both in the notifications and in the failure report (#486). |
| Updater manifest (`updater/latest.json`) is behind the newest published `v*` release | Compare the manifest version with the newest non-draft published release and report both versions in the notifications (#521). |
| Release gate refused because the release commit has no green `velocity/integration` status | Publish a local green pass or start an immediate integration build covering that commit; once green, re-dispatch `release.yml` for that tag when its exact commit is green, dispatch the next free beta tag on the newer green commit for beta tags, or report that a release can be cut, recording handled runs across restarts (#521, #549). |

A bell at the right of the title bar shows the number of active signals, coloured by the most severe one.
Clicking the bell opens a dropdown listing every active signal.
Each row shows its severity, signal, automatic action taken or block reason, timestamp, optional detail line, and a link to open an owning thread when one exists.
When a signal appears or its text changes while the project is open, it also pops out under the bell for 8 seconds, at most three at a time.
Hovering a pop-out keeps it open, and opening the dropdown clears them.
Signals already active when a project opens only update the bell, so switching projects does not replay them.
When every signal is clear, the bell is muted and the dropdown says so.
