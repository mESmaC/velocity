#### PR duty · How a review runs · Projects never wait for each other, and Rosen work never waits for GitHub

<!-- split-spec: {"order":997,"kind":"markdown","indentation":2} -->
- **Projects never wait for each other, and Rosen work never waits for GitHub.**
  Each project is passed on its own, so one waiting on GitHub holds up no other; a project asked for again while its pass runs is passed again as soon as that ends.
  A Rosen project's pass does not list its PRs: it takes them as GitHub last listed them, and a listing is read again off the pass when it is older than 20 seconds or a PR's review ends.
  A listing older than 2 minutes is not acted on.
  A git project's pass lists its PRs first, since they are its work.
  Every `gh` call is stopped after 120 seconds, so a hung request ends as a failed one.
