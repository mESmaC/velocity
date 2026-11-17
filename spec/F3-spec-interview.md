<!-- split-spec: {"order":441,"kind":"markdown","indentation":0} -->
### F3 · Spec interview

1.
   **Scan:** the agent reads the folder or repo and lists what it found as tool steps.
   It works in the main checkout and gets **no workspace**.
   - The scan also suggests skill packs for the project (F18): UI UX Pro Max when it finds a UI, and Game dev skills when it finds a game engine.
     Suggestions are switched on unless you turn them off.
   - When UI UX Pro Max is on and the project has a UI, the interview asks the design questions it needs (product type, audience, mood) and generates the design system before the spec locks.
2.
   **Ask:** one question at a time.
   - Each question has a short "why it matters", up to 4 options (one marked *recommended*, answered with keys `1`–`4`), and a free-text answer box.
   - Each answer is recorded as a decision (key: value) and can add draft tasks to the Task List.
     Draft tasks show with a dashed icon.
   - The agent keeps asking until nothing is ambiguous.
3.
   **Ready:** a spec card lists every decision.
   - **Proof list:** the card proposes which features get Lean proofs (F17), and you edit it before locking.
     Most projects list a handful; UI and plumbing never appear on it.
   - **Lock spec & publish tasks** (`L`): commits the root spec and `spec/` sources together and pushes to the remote base before locking, turns the draft tasks into real tasks, and marks the thread done.
   - **SPEC.md** (`S`): opens the file.
   - **Keep grilling** (`G`): asks more questions.
4.
   **How the agent reports structure:** the interview runs on any provider, so the prompt carries the whole protocol.
   The agent ends each reply with a fenced `velocity-interview` JSON block: the scan summary (first reply), the draft tasks the last answer adds, and either the next question (`key`, `text`, `why`, up to 4 options, one recommended) or `ready` with the proposed proof list.
   Velocity hides the block from the transcript.
   A reply without one leaves the question open for a free-text answer.
   The agent reads the whole spec (F2) and preserves an existing split layout.
   Lock writes the edited proof list into its owning source file, or adds `## Proof list` to the root (the first source when no root exists), before committing all sources without flattening them.
   After a restart, an interview resumes on your next message from its saved decisions (§3.2 Resume after a restart).

**Accept when:**
- [ ] No build thread, swarm or dispatch can start on a project whose spec is unlocked.
      Swarm marks draft tasks "lock the spec first".
- [ ] Locking commits and pushes SPEC.md before it publishes the tasks.
- [ ] The Task List counter shows `draft · N` before lock and `done/total` after.
