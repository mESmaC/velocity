#### PR duty · How a review runs · A review lasts until its agent records a decision, not one turn

<!-- split-spec: {"order":999,"kind":"markdown","indentation":2} -->
- A review lasts until its agent records a decision, not one turn.
  Its prompt tells the agent never to end its turn before deciding and, for a review with local checks, to run long commands (a cold build, `lake build`, the stress suite) in the foreground.
  A turn that ends with no decision and no instruction of yours queued gets a reminder to carry on, or to say why it cannot decide; after 3 reminders the session stops and the head rests as above.
