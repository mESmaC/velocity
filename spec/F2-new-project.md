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

`Ctrl+Shift+N` opens the New project dialog.
- **Location:** choose an absolute folder path or use Browse… to open the native folder picker.
  The dialog suggests a dedicated `Projects` folder under your home folder and requires a typed or browsed project path before Create can use it.
- **Mirror:** choose Collapse, an existing GitHub repository, or None.
  Collapse is the default once Collapse is connected in Settings → Agents → Shared MCP servers (#427).
  Leave the Collapse name blank to create a private repository named after the folder, or name one; Create provisions it before the initial import pushes the base.
  Provisioning creates the repository with the provisioning token, waits until Collapse reports it ready, mints a push token and stores it with `git credential approve` under the repository's URL (username `rosen`, `credential.useHttpPath=true`).
  Velocity also sets `credential.https://api.collapse.sh.useHttpPath` and `http.https://api.collapse.sh/.proactiveAuth` (`basic`) in the user's global git configuration so Rosen's pushes and authenticated reads of private Collapse repositories find the token (#571).
  It checks that Git 2.46 or later is installed before setting `http.proactiveAuth` and refuses with the minimum version when Git is older, or when git has no credential helper.
  The new `rosen.toml` also names the repository's `collapse` root, `https://api.collapse.sh/<owner>/<repository>`, so Rosen sends Collapse its live state and ledger entries and runs the owner's actions from Collapse (F5, #517).
  The push token is stored under the state feed and ledger URLs as well, where Rosen looks for it, and the new project's server allows the relay.
  Velocity never stores the push token, and reads it from the credential helper only to copy it to the state feed and ledger URLs of a project whose mirror already has one.
  Without a Collapse connection the dialog still accepts an existing `owner/repository` whose push token git already has, and copies that token to those URLs.
  The Collapse option also offers an optional **Issues and CI on GitHub** (`owner/repo`) field so a Collapse-mirrored project can name the GitHub repository used for issues, continuous dispatch, base CI, releases and Rosen pin bumps (Q28, #574).
  The same field is editable in Settings → Workspaces, where Velocity suggests a GitHub repository configured later as Collapse's downstream remote.
  Running Mirror to Collapse on an existing GitHub project preserves its `github_remote` pointing at GitHub so dispatch and CI keep working.
- **Model for the interview:** choose a model, effort and speed.
- **What are you building?** Enter a free-text brief.
- **Create & start interview** (`Ctrl+Enter`) creates the folder when needed and starts the Rosen server with an initial base.
  Velocity adds `rosen.toml` with after-land review, generated paths, registration lists, a warm pool and prepare settings when the file is missing.
  Velocity adds `AGENTS.md` from the default template when needed (F11).
  The project opens with its spec interview in a Rosen workspace.
- **Esc** dismisses the dialog without creating anything.

**Open project** in the project switcher (F1) opens or imports an existing project.
- **Open folder…** opens the selected Rosen project or imports the default branch of an existing Git checkout into a local Rosen server.
- **Open from GitHub…** lists the repositories you and your organisations own through signed-in `gh`, most recently updated first.
  Search filters the list by name and description, and typing an unlisted `owner/name` adds an exact choice.
  The clone destination is `<clone to>/<name>` and defaults to the dedicated `Projects` folder under your home folder.
  To reuse a matching checkout, enter its parent folder in **Clone to** or choose the checkout with **Open folder…**.
  Velocity imports a new or matching existing checkout into Rosen and uses GitHub as its mirror.
- A spec already on the Rosen base opens locked, including split specs under `spec/`.
  A project without a spec starts the interview in a Rosen workspace with Settings' default model, effort and speed.
  The interview waits for a provider sign-in when needed.
- Opening refuses a registered project, an unsupported local folder, an inaccessible GitHub repository or a clone destination occupied by unrelated files.
- Create and Open refuse home, drive and network-share roots and folders inside system directories, including AppData, Program Files and Windows.
  The refusal names the requested folder and comes before repository setup or template writes.

**Accept when:**
- [ ] Every successful Create and Open starts or reconnects to a Rosen server with a base before registering the project.
- [ ] Projects that need a spec run the interview in a Rosen workspace.
- [ ] An existing spec on the base opens locked.
