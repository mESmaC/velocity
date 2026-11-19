<!-- split-spec: {"order":1558,"kind":"markdown","indentation":0} -->
### F17 · Proofs and adversarial testing

**Why:** assume everything fails.
Tests written by the agent that wrote the code cost CI money, slow the builder down, and check what the builder already believed.
So the work is split:
- **Builders prove.**
  Lean proves the design is right and the code matches it.
- **Breakers test.**
  Scouts and review workers write every test and stress script.

Lean is the proving ground, not the test suite.

| Who | Writes | Never writes |
|---|---|---|
| Building agents (threads, swarms, goal swarms) | Code first; then proofs for proof-list features, and a targeted test only for a critical contract | Test suites, stress scripts, Lean for anything off the proof list |
| Scouts | Tests, stress scripts, issues with evidence | Fixes |
| Review swarm | Reviews of landed changes and proof verification | Pre-land approvals |

**Which features get proofs.**
Only the features on the project's **proof list**: design decisions where a mistake loses work, corrupts data or breaks coordination, and where there is a real choice between designs.
- **On the list:** algorithms, concurrency and coordination, data integrity, and anything destructive.
- **Never on the list:** UI, layout, styling, configuration, persistence formats, IPC, provider sign-in and toolchain glue.
  These ship with no Lean.
- **Who sets it:** the spec interview (F3) proposes the list and SPEC.md records it.
  Adding a feature takes your decision; an agent that thinks one belongs files an issue.
- **Why:** proving every feature turned Lean into a test suite.
  On Velocity's own first milestones, Lean grew to 43% of the repo, with theorems about single keyboard shortcuts and token values, while implementation stalled.

Velocity's own proof list:

| Feature | SPEC | What could go wrong |
|---|---|---|
| Claim lock | F14 | two agents hold the same piece or file |
| Targeted rebases and landing | F6 | an affected agent isn't rebased, or a merge drops a change |
| Workspace deletion | F5, F12 | a workspace with unpushed work, or another thread's, is deleted |
| Goal swarm work choice | F16 | a piece is taken twice, or an agent leaves while a piece is unclaimed |
| Stall detection and claim release | §2.2 | a live agent's claims are released, or a dead agent's never are |

**Implementation first.**
Proofs never block building.
The agent builds and runs the feature before its proof work.
Implementation stays the larger part of each change.

On after-land Rosen projects, proof-list verification follows the implementation's land in the Review swarm (F7, #382).
Its proof changes land independently, and its completion report is tied to the checked source tree.
Lean and review coverage never gate another land; full tests, `lake build` and the axiom audit gate the release.

**Design proof** (`proofs/<Feature>/Design.lean`, after the code, for every feature on the proof list):
- **Exactly three theorems:** `correct`, `cost_bound` and `optimal`, below.
  Lemmas are allowed only as steps toward them.
- **No theorem about a single behaviour, case, key, value or example.**
  That is a test.
- **Size tripwire:** a design proof over 300 lines means the model is too detailed.
  The agent stops and files an issue.
- **Model:** the feature as Lean definitions: its state, its operations and the SPEC.md section it implements, stated as a `Spec` predicate.
- **`correct`:** the design meets `Spec` on every input.
- **`cost_bound`:** its cost stays within a bound, under a cost model the file defines (steps, allocations, rebases, round trips, whatever the feature spends).
- **`optimal`:** no alternative in a stated class meets `Spec` at a lower cost.
- **"Could not be any better" is proven against something stated.**
  Optimality only holds for a named cost model and a named class of alternatives.
  A proof against an unstated alternative proves nothing, so the statement must name both.
  "Cleanest" isn't a theorem; the reviewer judges it.

Example, from Velocity itself (F6):

```lean
-- proofs/TargetedRebase/Design.lean
theorem correct : ∀ m bs, ConflictFreeAtLanding (targeted m bs)
theorem cost_bound : ∀ m bs, rebases (targeted m bs) ≤ (bs.filter (touches m)).length
theorem optimal : ∀ strat ∈ RebaseStrategies,
    (∀ m bs, ConflictFreeAtLanding (strat m bs)) →
    ∀ m bs, rebases (targeted m bs) ≤ rebases (strat m bs)
```

**Confidence proof** (`proofs/<Feature>/Confidence.lean`, after the code): proves what was built matches the design.
Each language in the change reaches the strongest level available to it:

| Level | How | Available for |
|---|---|---|
| **code** (on request) | A translator turns the code into Lean, and the proof shows the translated code refines the design model. | Any language with a translator set for the project. Rust ships with Aeneas (its safe subset). More translators are added per language in Settings → Proofs (Q23). |
| **reference** (default) | The Lean model exports a compact fixture table, and one test checks the code against it. For larger input spaces, the model is compiled (Lean 4 compiles to C) and exported with `@[export]`. Scouts' stress scripts run the code and the model on the same inputs and diff the outputs. C and C++ link the model directly. Python, TypeScript and every other language call it through their FFI or a small process. | Every language |
| **model** | Only the design is proven. The PR is flagged, and scouts treat it as a proof gap. | Every language |

**How Velocity reads the level** *(proposed)*.
For each proof-list feature a PR touches (a file in `proofs/<Feature>/`, its root module or its fixture table, where `<Feature>/Design.lean` exists), Velocity reads the checkout without building anything:
- **code** for Rust when `proofs/<Feature>/Generated/` holds the Aeneas translation and `proofs/<Feature>/Confidence.lean` exists;
- **reference** for each language whose tracked code outside `proofs/` names the feature's fixture table `proofs/fixtures/<feature>.tsv` (the folder name in kebab case: `TargetedRebase` → `targeted-rebase`), a test or a stress driver;
- **model** otherwise, listed for each language the PR changes, or once with no language when it changes no code.

A language's strongest level is code where a translator exists (Rust: Aeneas) and reference everywhere else until Q23 adds translators.
The levels go on the proof check report with the other checks, one line per feature and language with its evidence.
A model-only level where a stronger one is available is flagged there and never fails the checks.

**Generated translations** (Aeneas output) are marked generated in `.gitattributes`, so they stay out of language statistics and PR diffs.

**Bug fixes:** no design proof.
In a proof-list feature, the confidence proof must cover the input the scout's evidence failed on.
Elsewhere, a fix has no Lean.
The fixing agent copies the scout's script into `stress/` unchanged as part of its PR.
Copying isn't writing: Velocity checks the file matches the issue's attachment byte for byte.

**What proof workers check after a land:**
- `lake build` succeeds for the affected proof files.
- No `sorry` or `admit`.
- `#print axioms` lists only `propext`, `Classical.choice` and `Quot.sound`.
  This also catches `native_decide`, which adds an axiom of its own, and any `axiom` declaration.
- The design proof names the spec section it covers.
  Existing `SPEC.md F5` or `SPEC.md §3.1` references keep working when the section moves.
  Direct `spec/` file references name an existing source and a heading ID in the project spec, either explicitly or in the source filename (for example `spec/F5-worktrees.md`).
  Proofs settings reads the proof list from whichever source file holds it.

How the checks run:
- Review workers verify proof-list features at their landed commit and name affected spec sections.
- **Build.**
  A project on Mathlib runs `lake exe cache get` first, so no workspace compiles Mathlib from source.
  Then `lake build`.
- **Axiom audit.**
  `#print axioms` in a file covers only what its author chose to print, so Velocity elaborates its own audit file with `lake lean`: it imports a changed module and lists the axioms of every declaration it defines.
  Each changed module gets its own audit file, because modules that are fine alone can clash when imported together (two `Export.lean` files each define the top-level `main` their `lean_exe` needs).
**Where it runs:**
- Proofs and stress scripts run locally, in each agent's workspace, on the machine that already runs the agents.
- Tests and stress scripts live in `stress/` (and the project's own test folder).
  Scouts run them on sweeps, and the release gate runs the full stress suite.
  They never run in per-PR GitHub CI.

**Proof card** (in the land history, F6):
- the proof folder, and "N theorems proved";
- the check results for the build, placeholders and axiom audit;
- every theorem: Design or Confidence, its name and its statement;
- the level reached per language, e.g. "Rust · reference · fixture table checked by one test", "TypeScript · reference · scouts diff 10,000 stress inputs against it".

How the card is made *(proposed)*:
- **Where and when.**
  For each new head of a thread's named change, Velocity runs the checks above in the background in the thread's own workspace, once that workspace is checked out at exactly the head with no edits under `proofs/` (otherwise the next observation retries).
  The card sits under "PR opened" in the land history; a PR that touches no proof file, fixture table or proof-list feature has no card.
- **Track.**
  The "Proved" step runs while the head is unchecked, is done when the checks pass and fails when one fails.
  It reads "No proofs" when the PR changes nothing to prove.
  A pass ticks "· proved" on the collapsed work line and posts a `proved` event naming the proof folders, the theorem count and each language's level.
- **Levels.**
  One row per feature and language, read as "How Velocity reads the level" says; a flagged model-only level is shown in amber.
  A PR that changes only a proof-list feature's fixture table still gets a card, with its fixture rows and levels.
- **`stress` events.**
  Each issue a scout files posts a `stress` event naming its evidence script.

**Accept when:**
- [ ] A building agent's change adds tests only for critical contracts, and its implementation outweighs its proofs, tests and docs together.
- [ ] The review swarm checks a proof-list feature's design proof after the implementation lands.
- [ ] A change for a feature off the proof list contains no Lean.
- [ ] The proof checks flag `sorry`, `admit`, `native_decide` and any disallowed axiom after landing.
- [ ] Every issue a scout files carries a failing test or stress script.
- [ ] The proof card shows each language's level, and a model-only level is flagged when a stronger one exists.
