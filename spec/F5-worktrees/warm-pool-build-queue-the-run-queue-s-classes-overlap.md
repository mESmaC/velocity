#### Warm pool (speed) · Build queue · The run queue's classes overlap on purpose

<!-- split-spec: {"order":750,"kind":"markdown","indentation":2} -->
- The run queue's classes overlap on purpose.
  One run per class at a time, machine-wide: a landing check, an agent's build, the tests after a land and a warm pool prepare can all run at once, the less important ones at below-normal process priority so the landing check gets the cores first.
  Two projects' landing checks on one machine take turns in the one `landing` class, so a second project's land waits for the first's check.
  Process priority shares the CPU, not memory or the disk: overlapping runs still contend for those.
