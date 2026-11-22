<!-- split-spec: {"order":115,"kind":"markdown","indentation":0} -->
## 3. Architecture

- **Shell:** Tauri v2, frameless window (`decorations: false`) with a custom title bar.
  Drag regions use `data-tauri-drag-region`; the window controls are custom.
  The window keeps the system frame shadow (`shadow: true`) and a dark window theme, so on Windows 11 it has the system's rounded corners and dark 1 px border; maximized, it fills the work area with square corners.
- **Frontend:** runs in the webview: Svelte 5 + TypeScript on Vite (no SvelteKit), built with pnpm.
  Command and event types are generated from Rust with `tauri-specta`.
  All visual values come from the reference UI: its `:root` is copied exactly, and missing values are copied from its winning CSS declarations into supplemental reference tokens (§6).
- **Backend (Rust)** owns:
  - agent processes
  - Rosen servers and workspaces
  - GitHub access
  - the file system
  - persisted state

  The frontend never runs a shell command.
  It calls Tauri commands and subscribes to events.
- **Agent providers:** Anthropic (Claude Code), Codex, Antigravity and Kimi Code.
  Every provider signs in with OAuth on your own subscription, the way T3 Code does.
  Velocity talks to each provider's own agent over ACP (§3.2) and never asks for or stores an API key (F15).
  Each agent records its provider, model, effort (Low / Medium / High / Max) and speed (Regular / Fast, F15).
- **GitHub:** issues, the git mirror and base CI.
