#### Rosen · Headless machines (#270)

Settings → Worktrees → Machines enables a per-project Rosen TLS listener and creates join keys through `rosen-client` with an advertised address, expiry in seconds, use limit and role (agent machine or checker).
It lists machines with their role, status, last contact and running agent names.
Pending machines can be approved with one click; enrolled machines can be revoked there.
The listener address is saved with the project's Rosen data and survives restarts.
Enabling or changing it restarts that project's server; its followers and workspace watchers reconnect.

A provisioned Windows machine runs `velocity-app machine join --key <key> --data-dir <folder> [--name <name>] [--slots 40]`.
That one foreground command verifies the key's certificate fingerprint, enrolls with Rosen, starts its local cache daemon and registers for Velocity dispatch on Rosen's authenticated project channel.
It opens no window or login.
The OS account supplies its already provisioned OAuth sign-ins and tools.
Rosen stores the machine credential in Windows Credential Manager; Velocity retains only the machine ID.
The join key is displayed only in memory and never saved.
Restart the worker with `velocity-app machine run --data-dir <folder> [--slots 40]`, including under a service manager; a second worker cannot own the same data directory.

The Machines panel dispatches a prompt to an active registered machine only after the project spec is locked.
The worker durably records each channel message before starting its job, deduplicates redelivery by message number, and runs up to its configured slot count through the existing unrestricted ACP client, using that account's Velocity provider settings.
Each job has its own Rosen workspace and write-through watcher; the session declares the agent name and role as `<role>-<message number>`.
All sessions use the machine's credential through its cache daemon.
Completed turns report their result and change ID in the panel.
Queued jobs and session IDs survive a restart.
Revocation refuses both dispatch and Rosen access under the same credential.
Loss of authenticated upstream access stops running agents within the worker's request timeout and polling interval (15 seconds plus 2 seconds); their durable jobs and workspaces remain for recovery.
Machine roles identify their purpose and do not restrict agent tools.
