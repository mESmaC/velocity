#### PR duty · How a review runs · A Rosen change · Velocity checks and lands the approved head · The folder holds exactly the merge result's files, plus build output

<!-- split-spec: {"order":1011,"kind":"markdown","indentation":6} -->
- The folder holds exactly the merge result's files, plus build output.
  A file the last result had and this one lacks is removed; one that can't be removed, such as a file another program holds open, stops the check, which is retried on the next tick, and stays listed so that check removes it.
