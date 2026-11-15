<!-- split-spec: {"order":396,"kind":"markdown","indentation":0} -->
### F2 · New project

**Project spec layout (#400).**
Velocity reads the project's root `SPEC.md` plus all Markdown (`.md`, case-insensitive) files recursively under `spec/` as one spec, including section folders' `README.md` files.
The root is read first, then folder files in relative-path order; directory symlinks are not followed.
A project with only `SPEC.md` behaves as before, and a `spec/` folder can supply a spec when the root file is absent.
The saved `spec_path` continues to name the root file (default `SPEC.md`); configured roots still include the project's `spec/` files.
Sections are identified by their heading IDs, such as F5, §3.1 and R8, whichever file holds them.
All spec consumers use this layout: work inventory and goal swarms, Proofs settings and design-proof references, design-system generation, opening projects, the interview and Rosen's source-issue notes.
New projects still start with a single `SPEC.md`.

`Ctrl+Shift+N` opens a dialog with:
- **Location:**
  - **Local folder:** a path and Browse… (native picker via `tauri-plugin-dialog`).
    One option: "create the folder and run `git init` if needed".
  - **GitHub:** New repo or Existing repo; `github.com/<owner>/<name>`; Private or Public; and a "clone to" path.
- **Model for the interview:** model cards plus effort and speed.
- **What are you building?**
  A free-text brief.
- **Create & start interview** (`Ctrl+Enter`).

**Accept when:**
- [ ] Create sets up the project (folder / `git init` / repo / clone), switches to it, and starts the spec interview thread straight away.
- [ ] Esc cancels; nothing is created.

**Open project** (in the project switcher, F1): opens a project that already exists, without the dialog.
- **Open folder…:** the native folder picker.
  The folder must already be a git repository; it is opened where it is, nothing is cloned or initialised.
  When its `origin` is on GitHub, the project is shown as a GitHub project.
- **Open from GitHub…:** the dropdown shows your repositories in place of the project list: those you own and those of organisations you belong to, read through signed-in `gh`, most recently updated first.
  A search box filters them by name and description; typing an `owner/name` that is not in the list offers it as its own row.
  A "Clone to" folder (default: your home folder) sets where the clone goes, `<clone to>/<name>`.
  Choosing a repository clones it there, or opens the clone already in that folder when its `origin` is that repository.
- **The spec:** a project that already has `SPEC.md` at its root opens with its spec locked, so build work can start at once.
  A project without one starts its spec interview, as Create does (Rule 2), using Settings' default model, effort and speed and no brief.
  The interview's scan reads the existing code; it waits for a provider sign-in if none is signed in.
- The existing spec can also be supplied by `spec/` Markdown files (Project spec layout above); Open reads all sources before adopting it.
- Opening refuses, before anything is cloned, a folder that is already a project, a folder that is not a git repository, a repository `gh` cannot see, and a clone folder that is occupied by something else.
  As with Create, a missing AGENTS.md is added from the default template and committed (F11).

**Accept when:**
- [ ] Open folder… opens a git checkout from the native picker and switches to it.
- [ ] Open from GitHub… lists your and your organisations' repositories, searchable, and opens the chosen one by cloning it or by opening its existing clone.
- [ ] A project with `SPEC.md` opens with its spec locked; one without starts the spec interview.
