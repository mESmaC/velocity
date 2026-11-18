#### Review swarm on after-land Rosen projects (#382) · For each affected F17 proof-list feature, the worker writes or updates proofs/<Feature>/Design.lean and the exported fixture table, with the implementation's fixture check

<!-- split-spec: {"order":1088,"kind":"markdown","indentation":0} -->
- For each affected F17 proof-list feature, the worker writes or updates `proofs/<Feature>/Design.lean` and the exported fixture table, with the implementation's fixture check.
  `review_swarm_complete` requires the feature's design and fixtures and queues `lake build`, the placeholder scan, SPEC-section check and axiom audit in Rosen's `verify` queue.
  The worker ends its turn after queuing and Velocity resumes it with the result and full log path.
  Model/code gaps become issues with evidence.
  Proofs and fixes are ordinary named Rosen changes; they land through the same after-land policy, with no approval or receipt.
  A failed proof never delays another land.
