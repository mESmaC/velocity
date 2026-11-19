# Retired stress scripts

Removed by the Rosen snapshot `Retire PR duty and keep Rosen review after landing (#424)`.

| Script | Risk | Reason for retirement |
| --- | --- | --- |
| `pr147-gate-after-checks.ps1` | Regenerating bindings could make an ordinary PR's checked checkout appear dirty and block its proof gate. | The application no longer prepares or gates GitHub merges. |
| `pr134-false-trivial.ps1` | A code change misclassified as trivial could go through the abbreviated PR review. | Velocity no longer picks a review depth or grants a landing decision for GitHub PRs. |
| `rosen-check-policy` | A moving base could certify an old check set or tier a sensitive path against stale configuration. | Velocity no longer certifies pre-land check results or decides review depth for named Rosen changes. |
