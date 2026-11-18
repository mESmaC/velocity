#### Warm pool (speed) · Build queue · The run queue's classes overlap on purpose

<!-- split-spec: {"order":750,"kind":"markdown","indentation":2} -->
- The run queue's classes overlap on purpose.
  Landing checks, agent checks, integration builds and warm pool prepares have separate priorities in Rosen's machine queue.
  Runs pass the machine's free-memory gate before starting.
  Integration builds hold a machine-wide single slot; agent light Rust runs share two slots of their own.
