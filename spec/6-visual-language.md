<!-- split-spec: {"order":1844,"kind":"markdown","indentation":0} -->
## 6. Visual language

- **Base:** OLED, Apple-inspired **graphite** with glassy panes.
  Black is true black.
  - **Window:** opaque `#000`.
    There is no desktop show-through (no Mica, Acrylic or vibrancy) because true black is the point.
    - Two faint pools of light sit behind the panes (`--ambient`: the accent at the top-left, the duty violet at the bottom-right) so the glass has something to refract.
      Everything else stays black.
  - **Layout:**
    - The sidebar is one floating glass pane, and each rail panel is its own pane.
    - The main column has no panel: content sits straight on the window.
    - The title bar is 40 px: the brand and project switcher on the left, the Focus / Board switch and the project pulse in the centre, and a search field that opens the command palette on the right.
    - The thread header has two lines: the title (18 px) with Tokens, Diff and Editor beside it, and the meta line (branch, workspace, diff counts, time) as chips under it.
    - Side panes narrow as the window narrows, and the rail collapses below 76 rem of window width.
    - The sidebar collapses below 54 rem.
    - Threads and Activity buttons open the collapsed panes, and their keyboard shortcuts open the pane before focusing the search or task entry.
    - The Board wraps its lanes into fewer columns as space runs out.
  - **Materials:**
    - Panes are smoked glass:
      - a translucent top-lit fill `rgba(255,255,255,.072)` → `.034`, plus a faint top-left sheen, over `blur(36px) saturate(1.7)` of whatever is behind the pane;
      - a 1px `rgba(255,255,255,.14)` edge and a 1px inner top highlight at 20%;
      - a soft `0 18px 50px rgba(0,0,0,.55)` lift, so panes float.
    - The composer takes an accent edge and a 3px accent ring on focus; chips brighten their rim on hover.
    - Popovers, menus, dialogs and toasts are opaque graphite (`--glass-pop`, `#25252a` → `#17171a`) with a 12% hairline and a deep shadow, so text behind them never shows through (#492).
      The scrim dims to `rgba(0,0,0,.58)` and blurs slightly.
    - Cards in the content area take a faint top-lit fill and a `rgba(255,255,255,.09)` hairline.
      Tool steps sit together in one such card, and the running step is an accent-tinted row.
  - **Shape:** corner radii 4 / 6 / 8 / 12 / 16 px (`--r-xs` … `--r-xl`).
    Panes and menus are 12 px, the composer and dialogs 16 px, cards 8 px, buttons and fields 6 px.
    Pills and toasts are fully round.
  - **Controls:** the accent is ion blue `#6a8dff`, used for the selected thread, focus rings, checkboxes, the primary button, the send button and anything running.
    `--accent-soft` (16%) fills and `--accent-line` (46%) rims are its two tints.
    - Thread rows are source-list rows.
      The selection is a small accent-tinted lozenge with an accent rim, not an outlined box.
    - Segmented controls use a sliding raised segment, and buttons are subtle raised fills.
      Buttons, segmented controls, fields, checkboxes and tags are shared controls (`system.css`), so they look and size the same on every screen; a label inside a control never wraps.
    - The send button is an accent filled circle with a black arrow, and shortcuts are keycaps.
  - **Type:** Inter (bundled), with JetBrains Mono (bundled) for code, paths and ids.
    Labels, counts and prose are never set in mono.
    - The scale in `tokens.css` is 11 / 12 / 13 / 15 / 18 / 22 px (`--fs-xs` … `--fs-2xl`); body text is 13 px.
    - Spacing steps are 2 / 4 / 6 / 8 / 10 / 12 / 16 / 20 / 24 / 32 px, and controls are 20 / 24 / 28 / 34 px tall (`--h-xs` … `--h-lg`).
      Buttons and rows are at least 28 px; compact buttons inside rows and cards are 24 px.
    - Counts that sit in columns or tick in place use tabular figures; body text does not.
    - UI zoom scales the whole window's web view, so typography, controls, spacing and pane breakpoints scale together on top of Windows display scaling (#495).
    - There are no italics anywhere, including placeholders.
    - Hierarchy comes from weight and the label greys: titles `#fff`, primary `#f5f5f7`, secondary `#b4b4bd`, tertiary `#8a8a91`.
      The tertiary grey still passes 4.5:1 on black.
- **State colours:** panes stay neutral.
  Colour marks three things only: the selection and primary action (the accent), the state a thread is in, and the provider that runs it.
  A state's colour appears on its title-bar count (a tinted pill), its sidebar group label and its row icon.

  | Colour | Use |
  |---|---|
  | ok `#5fd08a` | merged / done |
  | warn `#f0b651` | named · watching / CI / fixing |
  | err `#ff7369` | failed / changes requested |
  | accent `#6a8dff` | running |
  | info `#8fb4e8` | spec |
  | duty `#b79cff` | duty agents |
  | swarm `#5fd4c4` | swarm |
  | claude `#d97757` · codex `#e9e9e9` · agy `#8ab4f8` · kimi `#f08fb8` | provider marks (Anthropic, Codex, Antigravity, Kimi Code) |

- **Motion (#226):** gentle and quick, one system for the whole app.
  Nothing bounces, overshoots or calls attention to itself.
  - The `--motion-*` tokens in `reference.css` set four durations: quick 120 ms, fast 150 ms, base 180 ms and slow 260 ms.
    The 150 and 180 ms values come from the reference's toggle and toast.
    They also set two curves: an ease-out for things arriving and an ease-in for things leaving, which use the quick duration.
    Travel is 4 px and scale starts at 0.97.
  - Menus and popovers (the model picker, the thread context menu, the slash and `$` menus, and the project switcher) fade and scale in both directions.
    Modal dialogs (the palette, AIRC, Usage, Updates, New project, Duty and the swarm dialogs) fade and scale in both directions, with their scrims (#446).
    An overlay reverses smoothly when it changes direction mid-transition.
    Escape acts immediately and outgoing overlays release input as soon as they start to close.
    Focus returns to an available opener unless it has already moved elsewhere.
  - New rows rise 4 px into place: sidebar threads, tasks, issues, an issue's expanded detail, tool steps and other transcript blocks.
    Rows leave at once, so a list never shows old and new rows together.
  - Toasts fade and slide in both directions.
    Tooltips fade in both directions.
  - The arriving view rises into place when switching between thread, board, swarm, duty, settings and settings tabs.
  - Buttons, pills and rows ease their colour on hover and focus.
    Compact controls (pills, buttons, segmented controls) sink to 0.97 while pressed; full-width rows only change colour.
    Status dots and icons ease between state colours.
    The usage and limit bars fill when they appear and when their value changes.
  - Motion never delays input or reading.
    Agent text appears at once, typing never waits, and a backend update never restarts an animation: entry motion runs only when an element is inserted.
  - Motion uses opacity and transforms, apart from the small bars' fill, so nothing reflows.
  - With `prefers-reduced-motion`, nothing moves or scales and the bars don't fill.
    Short fades stay.
- **Long content and controls (#415):** long titles, project names, tool arguments and results, and diff paths truncate within their pane and expose the full value in a tooltip.
  Code, diffs, prose and error details wrap long lines without widening a pane.
  Panes scroll vertically when content exceeds their height.
  Dialog bodies scroll within the window while the header and action footer remain visible.
  Keyboard focus keeps the accent ring on raised controls, disabled actions are dimmed, and Settings navigation uses the shared hover state.
- Base tokens are copied exactly from the mockup's `:root` into `ui/src/lib/theme/tokens.css`.
  Shared controls live in `ui/src/lib/theme/system.css`, and the remaining per-component values in `ui/src/lib/theme/reference.css` (#12).
  Components use the token files; the reference is read-only.
  The Rust theme module mirrors the base tokens.
- Fonts are bundled locally in the app.
  The mockup uses a CDN only for convenience.

---

### Pane states (#416)

Sidebar threads, Changes / pull requests, Issues, every AIRC tab, Machines, Settings → Workspaces / Providers / Skills, the first transcript update and Usage share one pane-state component.
Loading shows neutral skeleton rows with an accessible loading label.
Empty copy names what is missing and how to add it or select its project; an empty search says that no threads match.
Failures name the affected data and one recovery action, never backend JSON, stack traces or command stderr.
GitHub authentication failures direct the user to `gh auth login` and Retry.
Retry refetches data, and failed reads retry every five seconds while mounted; automatic retries never replay actions that mutate data.
A failed agent response offers a manual Retry through its existing workspace retry or thread continuation action.
A network loss shows an offline state and retries reads on return.
The Rosen event follower publishes its connection state per project; affected panes show Reconnecting while it retries, retain their last snapshot, and recover when the stream opens again without restarting Velocity.
