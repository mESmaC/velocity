#### PR duty · How a review runs · A Rosen change · Velocity checks and lands the approved head · A successful land is checkpointed as merged before the checker schedules again, only while the thread still represents the same named work

<!-- split-spec: {"order":1013,"kind":"markdown","indentation":6} -->
- A successful land is checkpointed as merged before the checker schedules again, only while the thread still represents the same named work.
  A later lifecycle event or fixing state wins over a delayed land reply: its recovery message, pending follow-ups and reopened tasks are preserved, even across a restart.
  The event still owns the cursor and cleanup, but a stale review roster cannot re-submit reverted work while that event catches up.
