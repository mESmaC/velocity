#### Scouts · Focus areas · Development speed

<!-- split-spec: {"order":1098,"kind":"markdown","indentation":2} -->
- **Development speed (#622):** aimed at the tool suite itself — Velocity, Rosen and Collapse — preferring removing a step, a wait or a manual check over adding a feature.
- Velocity runs at most one machine-wide improvement scout at a time when dispatch slots have been free for 15 minutes with nothing dispatchable or when 25 lands have happened since the last run, skipping a run when the previous one filed nothing and no new signal has arrived since.
- Each run receives one compact digest containing `fleet_stats` rollups and top offenders when available, friction and tool-request groups below the filing threshold, `velocity.log` signals since the last run (project health skips and idle spells, perf-budget overruns, stale claims, failed restarts and post-land failures), `rosen stats` when available, and measured outcomes of earlier `self-improvement` issues.
- The improvement scout files at most three issues per run, each in the right tool repository, labelled `self-improvement` (creating the label if absent), with concrete evidence, duplicate checks against open and closed issues, and a `Target metric:` line naming the fleet metric and direction.
- Dispatch starts ready `self-improvement` issues ahead of other ready issues.
- For each landed issue with a `Target metric:` line, Velocity compares that `fleet_stats` metric in the same project before and after the land over a window of 24 hours or 20 lands, whichever is longer (#621).
- Once the window closes, Velocity posts an idempotent outcome comment on the issue with the before value, after value, change, and sample sizes, persists the outcome so the improvement scout reads past results, and lists recent improvements and their measured effect in the Fleet section.
- If the target metric clearly regressed (at least 25% worse with at least 10 samples on each side), Velocity files a follow-up issue labelled `self-improvement` that links the land and describes what regressed without reverting automatically.
