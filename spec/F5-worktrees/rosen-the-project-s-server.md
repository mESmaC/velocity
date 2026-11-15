#### Rosen · The project's server

<!-- split-spec: {"order":535,"kind":"markdown","indentation":0} -->
- **The project's server.**
  Velocity starts and owns one Rosen server per project, on this machine, with its data in `%APPDATA%\dev.velocity.ide\rosen\<project>` and its own named pipe.
  A release carries `rosen-server` and `rosen` inside Velocity, built from the Rosen commit Velocity pins, and unpacks them on first use into `%LOCALAPPDATA%\velocity-rosen\<content hash>`, so the portable download has them and an update never overwrites a running server.
  A development build finds them in the folder `VELOCITY_ROSEN_BIN` names, beside Velocity's executable, or on `PATH`.
  The first time, it imports the project's history from the checkout's `origin` (the checkout itself when it has none), and `origin` becomes the mirror every land is pushed to (Rosen R1).
  Velocity stops the servers it started when it exits and before an update installs, so their binaries can be replaced.
  Velocity follows every Rosen project's `mirror` events, including projects with no threads, and restores the latest mirror state at startup even when its thread-event cursor has passed the failure (#402).
  While the latest push has failed, the main window's project header and Settings → Worktrees → Version control show `failing · retrying` with the next retry time, or `rejected · needs a person` when Rosen says the push was rejected.
  Both show the error's first three lines and the last commit the mirror accepted, or say that no accepted commit is recorded.
  The first rejection in an incident raises one toast naming the project and writes one `failed` entry in its AIRC channel, addressed to the current duty agents.
  Retrying the same rejection does not repeat the toast or channel entry, including after a Velocity restart; a channel failure leaves a durable notice to retry without hiding the project warning.
  The warning clears on the next successful push, including a recovery after a server restart, and a later rejection starts a new incident.
