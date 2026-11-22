<!-- split-spec: {"order":1284,"kind":"markdown","indentation":0} -->
### F13 · Settings

Open with the footer gear, `Ctrl+,`, the palette, the footer workspace count (→ Workspaces) or the usage panel's Settings · Providers link (F15 Usage panel).
Pages: **Workspaces** (F5), **Providers** (F15: sign-in, default model, effort and speed for new threads, and the thread title model), **Appearance**, **Skills** (F18), **Agents** and **Proofs**.
Appearance shows the current UI zoom with controls to step from 90% to 150% in increments of 10 percentage points or reset to 100%.
The same zoom applies across projects and is restored on restart.
Proofs holds the project's proof list, the code-level translator for each language when an issue asks for one (Rust: Aeneas), and the stress-diff input count (F17).
Agents holds the shared MCP servers passed to every agent (§3.2 Tool access), the per-agent caps (max turns, max duration; off by default), the stall timeout (5 min, and 30 min for a running tool call) and the claim-release grace period (10 min) from §2.2.
**Trim unused agent context** defaults on and applies to new Claude Code and Codex agent processes started by Velocity (§3.2 Tool access, #458).
Agents also holds the experimental context hand-off switch (#467), which defaults off and applies to new Rosen work items.
**Auto-restart into local builds** defaults on and restarts into the newest local build by itself when waiting issues need its agent tooling or when no agent turn has been running for 2 minutes (#601).
The rest of the Agents page is unspecified.
There is no Keys page: every provider signs in with OAuth.
