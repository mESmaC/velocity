#### Review swarm on after-land Rosen projects (#382) · Coverage, not gates

<!-- split-spec: {"order":1090,"kind":"markdown","indentation":0} -->
- **Coverage, not gates:** the Review header and sidebar show the number of unreviewed landed changes, the oldest age and the configured review window.
  The roster caps concurrent provider sessions; adding or dismissing agents changes that cap.
  Velocity starts more of those workers as the backlog grows or approaches its window.
  The Review swarm start dialog offers a review window of **1m** or **30m**.
  Reviews drain continuously with either window.
  Idle workers wait for landed changes.
  Instructions reach running workers at their next message, and later work follows the latest instruction.
  Pause finishes in-flight work and starts no new work; Stop or Dismiss stops the corresponding sessions and releases their review claims.
