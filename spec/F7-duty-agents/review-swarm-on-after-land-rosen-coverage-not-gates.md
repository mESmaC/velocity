#### Review swarm on after-land Rosen projects (#382) · Coverage, not gates

<!-- split-spec: {"order":1090,"kind":"markdown","indentation":0} -->
- **Coverage, not gates:** the duty header and sidebar show the number of unreviewed landed changes, the oldest age and the configured review window.
  The roster caps concurrent provider sessions; adding or dismissing agents changes that cap.
  Velocity starts more of those workers as the backlog grows or approaches its window.
  The Duty dialog's review-window choices reuse saved cadence settings: **1m** (after every land), **30m** (periodic), or **30m · scout once** (once).
  Reviews drain continuously in every mode; cadence controls idle scouting.
  When no review remains, a worker scouts adversarially in its own Rosen workspace.
  Instructions reach running workers at their next message, and later work follows the latest instruction.
  Pause finishes in-flight work and starts no new work; Stop or Dismiss stops the corresponding sessions and releases their review claims.
