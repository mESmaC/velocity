<!-- split-spec: {"order":2004,"kind":"markdown","indentation":0} -->
## 8. Open questions

| # | Question | Blocks |
|---|---|---|
| Q1 | Decided in #440: on Rosen projects, local Task List entries and GitHub issues share the Work panel, with status driven by the linked Rosen change (F8, F9). | — |
| Q2 | How is "the implementation matches the spec exactly" verified? | a spec-check feature |
| Q3 | Final names for "duty", "PR duty" and "Scouts". | labels only |
| Q4 | Does a swarm ever cap how many agents run at once, or always start everything? Built: everything starts. A per-provider cap is specified, off by default (F15). | F10 |
| Q5 | Does a running swarm pick up items added after launch? (Built: no.) | F10 |
| Q6 | Decided: it stays outside SPEC.md. Adding a task never starts an interview; a PR that changes specified behaviour updates SPEC.md itself (F8). | F3, F8 |
| Q7 | Decided: no. The spec interview writes SPEC.md only; AGENTS.md comes from F11. | F3, F11 |
| Q8 | Decided: the lint only advises. PR duty tightens a hedged line in an AGENTS.md change itself and never blocks on wording (F11). | F11 |
| Q10 | Ghost-text autocomplete while typing in AGENTS.md? (It would take Tab from indent.) | F11 |
| Q11 | Decided: Svelte 5 + TypeScript on Vite, built with pnpm (§3). | M1 |
| Q12 | Should AIRC be one channel per project, or one shared channel across projects (as in xros_chat)? Built: per project. | F14 |
| Q13 | Should agents be able to schedule jobs for each other through AIRC (xros_chat job queues)? | F14 |
| Q14 | Decided: Antigravity runs Google's own agent, `agy acp` (§3.2). Limits are read per provider outside ACP, from the sources T3 Code reads (F15 Limit sources). | F15, §3.2 |
| Q15 | Decided (#444): switch to another available account of the same provider, or wait for a reset when every account is exhausted (§2.2, F15). | F15, F10, F16 |
| Q16 | May goal swarm agents claim work outside the Task List, issues and SPEC.md (split a large item, or claim something they found)? Built: no. | F16 |
| Q17 | When there are more agents than unclaimed pieces, should the extras leave (built), or pair up on existing pieces (tests, review)? | F16 |
| Q18 | Should "keep going" be on by default? Built: off, which matches the prompt you use today (stop after the merge). | F16 |
| Q19 | Decided: yes. The four providers are built first, then any ACP agent can be added by command and arguments (F15 Custom ACP agents). | F15, §3.2 |
| Q20 | Decided: stall timeout 5 min (30 min for a running tool call; never in the build queue), grace period 10 min. A released agent's pushed branch is offered to the next claimer, never assigned (§2.2). | §2.2 |
| Q21 | Decided, then revised: PR duty reviews by reading and GitHub's checks on the approved head decide the merge. The local checks on the merge result decide it only for a sensitive PR, a PR that changes proofs, or a project with no GitHub checks (F7 "Checks before merge"). Proofs are checked locally before merge, and stress scripts run after the merge (F17). | F7, F17 |
| Q22 | Name for the "Main watcher" duty role. | F7 |
| Q23 | Which code-level translators should ship besides Aeneas for Rust (C, C++, Python and others)? Until one exists for a language, that language tops out at the reference level. | F17 |
| Q24 | Decided: no. The spec interview only proposes the proof list; each building agent writes its design proof after the code (Rule 8). | F3, F17 |
| Q25 | Decided: yes, a little faster. Providers count it inside their ordinary usage, with no category of its own (F15). | F15 |
| Q26 | Decided: Claude Code reads `.claude/skills/`; Codex and Antigravity read `.agents/skills/`, the Agent Skills standard folder both packs' docs name. Kimi Code's folder is unverified, so Velocity links into both `.kimi-code/skills/` and `.agents/skills/` until it is confirmed, then drops the extra row. The mapping is a data table in `velocity-skills`, so a correction changes one row (F18, #161). | F18 |
| Q27 | Should "Add a skill pack" also search the skills.sh registry, or take only GitHub repos? | F18 |
| Q28 | Decided: Velocity's own approval record bound to the head SHA, mirrored on GitHub as an "Approved at `<sha7>`" comment with a short reason; the approving reviewer then squash merges (F7, #42). Velocity's own repository uses the PR duty GitHub App (AGENTS.md). | M4 approve/merge, F7 |
