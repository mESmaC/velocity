#### Warm pool (speed) · Refilling

<!-- split-spec: {"order":696,"kind":"markdown","indentation":0} -->
- **Refilling.**
  The pool refills in the background.
  Ready worktrees fast-forward after each merge to `master`, then re-run setup only if the dependency manifests changed, and build the project at the new tip, so an agent's first build compiles only what it changes.
  A merge that can't change the build keeps a slot's prebuild: a slot built at the old tip stays built when the merge touched only prose and repository metadata (Markdown, reStructuredText, AsciiDoc, license and notice files, `.github/`).
  A license or notice file is one named `LICENSE` or `NOTICE`, optionally with a `-` or `_` qualifier, and with no extension or a prose one (`LICENSE-MIT`, `NOTICE.txt`); `NoticeBanner.svelte` or `licenses.json` is source.
  Both sides of a rename count.
  Any other file, or a diff that can't be read, rebuilds, and so does a slot whose prebuild failed, whatever the merge touched.

<!-- split-spec links -->

Rule files:

- [Built (proposed)](warm-pool-refilling-built.md)
