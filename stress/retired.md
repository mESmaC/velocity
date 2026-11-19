# Retired stress scripts

Removed by Rosen land `4a24ab9e4c8170587d41c27d8cd7a11ab222657a` (`Retire PR duty and keep Rosen review after landing (#424)`).

| Script | Risk | Reason for retirement |
| --- | --- | --- |
| `pr147-gate-after-checks.ps1` | Regenerating bindings could make an ordinary PR's checked checkout appear dirty and block its proof gate. | The application no longer prepares or gates GitHub merges. |
| `pr134-false-trivial.ps1` | A code change misclassified as trivial could go through the abbreviated PR review. | Velocity no longer picks a review depth or grants a landing decision for GitHub PRs. |
| `rosen-check-policy` | A moving base could certify an old check set or tier a sensitive path against stale configuration. | Velocity no longer certifies pre-land check results or decides review depth for named Rosen changes. |

Removed by the change for #426 (`Shrink velocity-github to issues and repository listing`).

| Script | Risk | Reason for retirement |
| --- | --- | --- |
| `pr50-stale-observer.ps1` | A GitHub PR read finishing after F12 deleted its thread could checkpoint stale PR state onto a replacement thread that reused the ID. | Velocity no longer reads GitHub PRs or saves a PR record on threads, so no late read can write thread state. |
| `pr64-follow-through-example.ps1` | Saving F9's issue association could break the F6 `follow_through` store example when every Cargo target is checked. | The PR follow-through example and the store API it called were removed with the PR code. |
