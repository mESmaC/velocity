<!-- split-spec: {"order":1197,"kind":"markdown","indentation":0} -->
### F11 · AGENTS.md editor

Open it from the rail link or the palette.
It reads and writes the repo's `AGENTS.md`.

- **An outliner, not a text editor:**
  - sections (`##`) with bullet rules and one level of sub-rules
  - inline code shown as code (backticks visible only while editing)
  - saved as plain markdown
  - a **Markdown** toggle shows the exact file agents read
- **Enter submits a bullet:**
  - A fast model *(proposed: Haiku 5.5)* rewrites it as a direct rule, using the whole file as context (e.g. it reuses the commands from `## Commands`).
  - The edit lands at once, with no approval, and the cursor moves to a new bullet.
    You keep typing while it works.
  - It can:
    - **tighten** ("You should probably keep functions under 60 lines."
      → "Keep functions under 60 lines.
      Split anything longer.")
    - **expand** short notes ("no unwrap" → "Never call `unwrap()` or `expect()` outside tests.")
    - **split** a bullet with two rules into two
  - If it cannot work out what a vague word means, it leaves the bullet flagged and says what is missing ("say what 'clean' means").
  - Every rewritten bullet shows "was ~~original~~ · **Undo**" until you save.
- **Instruction sections:** the new-project template and Covers **Add** share the complete Implementation first, Proofs, and Tests and docs bullets, including F17's theorem limits and fixture rules.
  Covers retains the reference's heading checks; adding a topic already covered leaves the draft unchanged.
  These are editable instructions, not approval gates.
- **Live lint while typing:** marks hedges (should probably, maybe, try to, where possible, as needed, consider), "should", and vague words (clean, good, nice, appropriate, reasonable, properly, etc.).
  - **The lint advises; it never blocks** (decided, Q8).
    It marks the words and lists them under Flagged.
    It never stops typing, saving or committing.
  - **AGENTS.md changes in a PR:** when a line the PR changed in AGENTS.md trips the lint, PR duty rewrites that line as a direct rule in the branch update it makes before merging (the same tighten rewrite as Enter), and says so in its review.
    It never requests changes for wording alone.
    A vague word it can't resolve becomes an issue (F7 severity bar).
- **Side panel:**
  - **Covers:** commands (build, proofs, lint); implementation first; proofs: proof list only, after the code; tests and docs: low priority; scope; PR: monitor until merged; PR: address every review comment; definition of done; hard "never" list.
    Each missing item has **Add**, which inserts editable bullets into the right section.
  - **Flagged:** the list, with **Tighten all**.
  - a key legend
- **Keys:**

  | Key | Action |
  |---|---|
  | `Enter` | submit |
  | `Shift+Enter` | new bullet, kept exactly as typed |
  | `Tab` / `Shift+Tab` | indent / outdent |
  | `Alt+↑/↓` | move bullet |
  | `Backspace` on an empty bullet | delete it |
  | `↑/↓` at the start/end of a bullet | move to the previous/next bullet |
  | `## Name` + `Enter` | new section |
  | `Ctrl+S` | save and commit |

- **Leaving a bullet** you changed also submits it.
- **External edits:** Save rejects a changed disk file and keeps your draft.
  Copy any edits you want to retain from Markdown, then choose **Discard draft and reload** to load the current file and resume editing and saving.
  Reload discards pending rewrites and never writes or commits; a read failure preserves the draft.

**Default installation:** New project setup installs the F11 template in the main checkout when `AGENTS.md` is absent and commits that file alone before registering the project and starting its interview.
Existing files (including empty files), directories and symbolic links are preserved.
The template is installed atomically, so concurrent setup cannot replace instructions.
Installation or commit failures stop setup and report the error; created instructions remain available for recovery.
On retry, a regular AGENTS.md whose bytes still match the generated default template is committed before registration, even when it was already present; custom instructions, directories and symbolic links remain untouched.
The interview continues to write only SPEC.md (Q7).
The template includes implementation-first, proof-list-only, scope, PR follow-through, Task List, AIRC, shared-file and review-severity rules.
Project-specific commands are filled in from the project's manifests and spec rather than assuming Rust or a frontend stack.

**The default AGENTS.md** includes these rules:
- Opening the PR does not finish the task; monitor it until it merges.
- Address every PR duty comment.
- Add to the Task List only when the user asks; otherwise file an issue.
- Review severity: what blocks a merge, and that everything else becomes an issue (F7).
- Shared files: the project's generated files and registration lists, and how to merge them (F14).
- Build queue: run every heavy build, test and lint through `rosen run -- <command>`, the machine-wide queue (F5).

**Accept when:**
- [ ] Saving commits and pushes AGENTS.md; agents pick it up on their next task.
- [ ] Undo restores your exact original wording, including removing any split-off bullet.
