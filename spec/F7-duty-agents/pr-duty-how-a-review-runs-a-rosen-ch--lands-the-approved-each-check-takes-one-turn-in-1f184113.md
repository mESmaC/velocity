#### PR duty · How a review runs · A Rosen change · Velocity checks and lands the approved head · Each check takes one turn in the machine's run queue (Rosen R3 "Run queue"), in its first class, so the checks of several projects never compile at once and no check waits behind an agent's build, the tests after a land or a warm pool's prepare

<!-- split-spec: {"order":1015,"kind":"markdown","indentation":6} -->
- Each check takes one turn in the machine's run queue (Rosen R3 "Run queue"), in its first class, so the checks of several projects never compile at once and no check waits behind an agent's build, the tests after a land or a warm pool's prepare.
  The feed's time for a check includes its wait.
