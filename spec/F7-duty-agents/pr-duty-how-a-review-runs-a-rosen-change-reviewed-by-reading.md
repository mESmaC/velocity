#### PR duty · How a review runs · A Rosen change · Reviewed by reading

<!-- split-spec: {"order":1006,"kind":"markdown","indentation":4} -->
- **Reviewed by reading** when the project's `rosen.toml` names pre-land commands and the change is not sensitive and does not touch `proofs/`.
  The commands and the sensitive paths are the policy Rosen reads from `rosen.toml` at the base, never from the change or from a git checkout's AGENTS.md, which can be older than the base; while Rosen can't say what the policy is, the change waits for the next tick.
  The agent reads the change and records its decision with `rosen_duty_change` and `rosen_duty_review`, its only tools.
  It builds nothing, its approval needs no receipt from it, and its session ends at its decision.
