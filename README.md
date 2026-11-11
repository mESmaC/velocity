<img src="assets/icon.svg" width="64" height="64" alt="">

# Velocity

Velocity is a desktop IDE that finishes projects fast by running many coding
agents in parallel, without stopping to ask for approval. It is built in Rust
and Tauri and runs on Windows 11.

**Status:** beta. The first release is
[0.1.0 beta 1](https://github.com/mESmaC/velocity/releases/tag/v0.1.0-beta.1).

## Why it exists

A single coding agent works on one thing at a time, and it stops whenever it
wants a permission or a decision. A real project has dozens of pieces that could
be built at once.

Velocity is built around that. You settle the spec once, then many agents build
it in parallel, each in its own checkout, and a piece counts as finished only
when its pull request is merged.

Running thirty agents on one repository raises problems a single agent never
meets: two agents taking the same piece, edits that collide, builds that starve
each other, and review that cannot keep up. Most of Velocity is the machinery
that handles those.

## What it does

- **Spec first.** A new project starts with an interview that produces `SPEC.md`
  and the first Task List. Build work starts once the spec is locked.
- **Threads.** A thread is one agent on one piece of work, in its own git
  worktree and branch. A warm pool keeps worktrees ready, so a thread starts in
  seconds.
- **Swarms.** A swarm starts one agent for every open task or issue. A goal
  swarm gives many agents the same prompt and lets each claim its own piece.
- **Coordination.** Each project has a channel (AIRC) where agents claim their
  work and the files they touch. The first claim on a piece wins. Overlaps are
  flagged while agents are still editing, and an agent that depends on another's
  pull request waits for it to merge.
- **Pull requests followed to merge.** An agent opens its pull request, then
  watches it: it fixes CI, answers review comments, and its worktree is removed
  when the pull request merges.
- **Duty agents.** PR duty reviews and merges pull requests. Scouts look for
  bugs and file each one with the failing script that shows it.
- **Four providers.** Anthropic (Claude Code), Codex, Antigravity and Kimi Code.
  Each signs in with its own OAuth login; Velocity never stores an API key.
  Settings → Providers installs a missing agent, and a usage panel shows how
  much of each subscription window is left.
- **Proofs where a mistake loses work.** A few algorithms, such as the claim
  lock and worktree deletion, are modelled and proven in Lean, and the Rust is
  checked against tables exported from the model.
- **Skills.** Skill packs are installed once and switched on per project.
- **Project panels.** A Task List, issues and pull requests, and an editor for
  the project's `AGENTS.md`.

`SPEC.md` describes every feature in full, and `design/main-window.html` is the
reference UI: open it in a browser to see the whole app with simulated data.

## Before you use it

- **Agents run unrestricted.** Velocity has no approval prompts. Every agent can
  edit files, run commands, use the network and use GitHub until it finishes.
  Use it on projects and machines where that is acceptable.
- **It is a beta.** Expect rough edges. Settings and saved state live in
  `%APPDATA%\dev.velocity.ide`; back that folder up before upgrading.
- **The downloads are not code-signed.** Windows SmartScreen warns on first run.

## Install

1. Download a build from the
   [releases page](https://github.com/mESmaC/velocity/releases). The `-setup.exe`
   file installs Velocity; the `-portable.exe` file runs without installing.
2. On first run, Windows SmartScreen shows a warning because the build is
   unsigned. Choose **More info**, then **Run anyway**.

You need:

- Windows 11, 64-bit. The WebView2 runtime ships with it.
- `git`, and the GitHub CLI signed in (`gh auth login`).
- Node.js 22 or later for the Claude and Codex agents. Velocity offers to
  install it when it is missing.
- A subscription with at least one of the four providers.

## Getting started

1. Open **Settings → Providers** and sign in to a provider. If its agent is not
   installed, **Install** adds it.
2. Create a project with `Ctrl+Shift+N`, or open an existing folder or GitHub
   repository from the project switcher.
3. A project with no `SPEC.md` starts with the spec interview. Answer its
   questions, then lock the spec.
4. Start a thread from the composer, or launch a swarm on the Task List and the
   open issues.
5. Start a PR duty agent from **On duty** in the sidebar, so pull requests are
   reviewed and merged as they arrive.

## How it is built

| Path | What it holds |
| --- | --- |
| `src-tauri/` | The Tauri app: commands and events only. |
| `crates/` | The logic, one crate per area: state, storage, git and worktrees, agent sessions, coordination, GitHub, threads, project creation, skills, the spec interview, and proof checks. |
| `ui/` | The frontend: Svelte 5, TypeScript and Vite. |
| `proofs/` | The Lean 4 project. |
| `stress/` | Adversarial tests and stress scripts. |
| `design/` | The reference UI. |
| `scripts/` | Development tooling: the local checks and the build queue. |

`SPEC.md` says what Velocity should do. `AGENTS.md` says how work on this
repository is done. Most of the code is written by coding agents following
those two files, many of them running in Velocity itself.

## Building from source

Install the Visual Studio C++ build tools, WebView2, rustup, Node.js 24 and
pnpm 10.34.9. Rust 1.99.0 is pinned in `rust-toolchain.toml`. Install the Tauri
CLI with `cargo install tauri-cli --version 2.11.4 --locked`.

From the repository root:

```powershell
pnpm -C ui install --frozen-lockfile
cargo tauri dev      # run the app; Tauri starts the frontend server itself
cargo tauri build    # build the app and its installers
```

A development run uses the same saved state as an installed build
(`%APPDATA%\dev.velocity.ide`), so do not run both at once.

Before opening a pull request, run the local checks. They run only the steps
your change can affect:

```powershell
pwsh -NoProfile -File scripts/check.ps1
```

The Lean proofs have their own setup; see
[the proof toolchain instructions](proofs/README.md).

## Contributing

Read `SPEC.md` and `AGENTS.md` first. `AGENTS.md` is written for coding agents,
but its rules for code, checks and pull requests apply to everyone.

## License

Velocity is licensed under the [Apache License, Version 2.0](LICENSE). It comes
with no warranty, and the license grants no right to use the Velocity name.
