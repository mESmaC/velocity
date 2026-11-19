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
  - A fast model *(proposed: Haiku 5.5)* rewrites it as a direct rule using the file as context, including the commands from `## Commands`.
    When the prompt includes an excerpt, the model can read the saved file for more context.
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
    It never requests changes for wording alone.
    A vague word it can't resolve becomes an issue (F7 severity bar).
- **Side panel:**
  - **Covers:** commands (build, proofs, lint); implementation first; proofs: proof list only, after the code; tests and docs: low priority; scope; Change: follow until landed; Change: address every review comment; definition of done; hard "never" list.
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
  | `Ctrl+S` | save and snapshot |

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
The template includes implementation-first, proof-list-only, scope, change follow-through, Task List, AIRC and shared-file rules.
It directs agents to the relevant spec headings using the graph tools and relies on the session for project instructions (§3.2).
Duty prompts carry the instructions for their roles (F7).
Project-specific commands come from the project's manifests or the applicable spec section.

**The default AGENTS.md** includes these rules:
- Name the change with `rosen commit -m`, then Velocity handles landing; the task finishes when the change lands.
- Resolve stored conflicts with `rosen rebase` and `rosen resolve`, and repair a reopened change after a post-land revert.
- Follow the AIRC session's Task List instructions.
  Continuous dispatch threads can add, edit and delete tasks on their own.
  Other threads change the Task List only on an explicit user request; otherwise file issues.
- Shared files: the project's generated files and registration lists, and how to regenerate them on combined inputs (F14).
- Build queue: use `queue_run` from `velocity-run` for commands that can take more than a minute, then end the turn and continue from Velocity's result (F5).

**Accept when:**
- [ ] Saving commits and pushes AGENTS.md; agents pick it up on their next task.
- [ ] Undo restores your exact original wording, including removing any split-off bullet.

The default template teaches Rosen workspaces, automatic snapshots, named changes, path claims and landing only.
Covers Add inserts the same Rosen instructions and never adds git commands or a GitHub PR workflow.
The legacy git-backed AGENTS.md save/publication implementation retains literal GitHub semantics until its owning migration changes it (#417).
