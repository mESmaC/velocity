#### PR duty · How a review runs · A Rosen change · Velocity checks and lands the approved head · When Rosen wants a receipt, it brings the project's pre-land-checks folder to the change's merge result and runs the pre-land commands there, in order, through the platform shell

<!-- split-spec: {"order":1009,"kind":"markdown","indentation":6} -->
- When Rosen wants a receipt, it brings the project's `pre-land-checks` folder to the change's merge result and runs the pre-land commands there, in order, through the platform shell.
  The folder is kept from one check to the next, and only files that differ are rewritten, so each build compiles what changed since the last one.
  A command may run for an hour.
