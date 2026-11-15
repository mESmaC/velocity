#### PR duty · The release gate

<!-- split-spec: {"order":1063,"kind":"markdown","indentation":0} -->
- **The release gate** (#381).
  On a Rosen project the release, not the land, is where everything is held to the full bar, like Scion's tests before a deploy (§3.3).
  `scripts/release-gate.ps1` runs on the release commit and stops at the first failure; the release workflow runs it before it builds anything, so a tag that fails it publishes nothing.
  Cutting a release requires:

<!-- split-spec links -->

Rule files:

- [the post-land full tests green on the release commit](pr-duty-the-release-gate-the-post-land-full-tests-green.md)
- [cargo run -p stress -- --all passing;](pr-duty-the-release-gate-cargo-run-p-stress-all-passing.md)
- [lake build in proofs/ passing, with the axiom audit (only propext, Classical.choice and Quot.sound);](pr-duty-the-release-gate-lake-build-in-proofs-passing-with.md)
- [no open issue labelled release-blocker](pr-duty-the-release-gate-no-open-issue-labelled-release-blocker.md)
