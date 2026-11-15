#### PR duty · How a review runs · A Rosen change · Velocity checks and lands the approved head

<!-- split-spec: {"order":1007,"kind":"markdown","indentation":4} -->
- **Velocity checks and lands the approved head** (Rosen §9 "Build queue").
  One checker runs per project, one change at a time, oldest first, as `velocity-checker`:

<!-- split-spec links -->

Rule files:

- [It asks Rosen to land the change first](pr-duty-how-a-review-runs-a-rosen-ch-and-lands-the-approved-it-asks-rosen-to-land-the-f39eb6c5.md)
- [When Rosen wants a receipt, it brings the project's pre-land-checks folder to the change's merge result and runs the pre-land commands there, in order, through the platform shell](pr-duty-how-a-review-runs-a-rosen-ch-lands-the-approved-when-rosen-wants-a-receipt-it-101b5029.md)
- [The commands, the check set the receipt names and the change's tier are read at the base of that merge result](pr-duty-how-a-review-runs-a-rosen-ch-ands-the-approved-the-commands-the-check-set-the-81d5073a.md)
- [The folder holds exactly the merge result's files, plus build output](pr-duty-how-a-review-runs-a-rosen-ch--the-approved-the-folder-holds-exactly-the-merge-3173977f.md)
- [Every command passed](pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved-every-command-passed.md)
- [A successful land is checkpointed as merged before the checker schedules again, only while the thread still represents the same named work](pr-duty-how-a-review-runs-a-rosen-ch-he-approved-a-successful-land-is-checkpointed-as-2dae756e.md)
- [It takes a change at its approval, without waiting for the reviewing agent's session to say its last words and stop](pr-duty-how-a-review-runs-a-rosen-ch--and-lands-the-approved-it-takes-a-change-at-its-ef1e5b5e.md)
- [Each check takes one turn in the machine's run queue (Rosen R3 "Run queue"), in its first class, so the checks of several projects never compile at once and no check waits behind an agent's build, the tests after a land or a warm pool's prepare](pr-duty-how-a-review-runs-a-rosen-ch--lands-the-approved-each-check-takes-one-turn-in-1f184113.md)
- [A command failed](pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved-a-command-failed.md)
- [The merge conflicts](pr-duty-how-a-review-runs-a-rosen-change-velocity-checks-and-lands-the-approved-the-merge-conflicts.md)
- [The checks could not run at all, or Rosen refuses the land for another reason](pr-duty-how-a-review-runs-a-rosen-ch-d-lands-the-approved-the-checks-could-not-run-at-e6a8c2dc.md)
- [This serial checker is retained for before-land projects](pr-duty-how-a-review-runs-a-rosen-ch-the-approved-this-serial-checker-is-retained-for-6520576b.md)
