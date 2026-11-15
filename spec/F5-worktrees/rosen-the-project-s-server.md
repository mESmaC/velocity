#### Rosen · The project's server

<!-- split-spec: {"order":535,"kind":"markdown","indentation":0} -->
- **The project's server.**
  Velocity starts and owns one Rosen server per project, on this machine, with its data in `%APPDATA%\dev.velocity.ide\rosen\<project>` and its own named pipe.
  A release carries `rosen-server` and `rosen` inside Velocity, built from the Rosen commit Velocity pins, and unpacks them on first use into `%LOCALAPPDATA%\velocity-rosen\<content hash>`, so the portable download has them and an update never overwrites a running server.
  A development build finds them in the folder `VELOCITY_ROSEN_BIN` names, beside Velocity's executable, or on `PATH`.
  The first time, it imports the project's history from the checkout's `origin` (the checkout itself when it has none), and `origin` becomes the mirror every land is pushed to (Rosen R1).
  Velocity stops the servers it started when it exits and before an update installs, so their binaries can be replaced.
