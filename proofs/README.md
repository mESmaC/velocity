# Proof toolchain

This is the prerequisite for the builders' design and confidence proofs in SPEC.md
F17. It does not implement the M11 proof UI, statement lock or merge gate.

The checked-in `lake-manifest.json` locks every dependency. Lean 4.35.0 and Mathlib
commit `56a005855ea17e0b0958f73ec801ae140b0a1679` match the Aeneas Lean backend at
`c5eb4a28cef65e0aa27743105308243e535a2a2b`. The translator bundle is the official
`nightly-2026.11.08-c5eb4a2` release, paired with Charon
`8cae8fd1905946de61147a65ecdd7122cc6b491b` and Rust `nightly-2026-09-17`.
The application will still use stable Rust; this separate nightly is only for
Charon's compiler integration.

## Windows setup

Install [elan](https://github.com/leanprover/elan), Git, and an x86_64 Ubuntu WSL
distribution with rustup available in its login PATH. Windows PowerShell 5.1 and
Windows' bundled `tar.exe` are sufficient. No Docker or OCaml build is needed.

From the repository root:

```powershell
powershell -File proofs/scripts/Setup-Toolchain.ps1
# A differently named Ubuntu distribution can be selected explicitly:
powershell -File proofs/scripts/Setup-Toolchain.ps1 -Distribution Ubuntu-24.04
```

The script checks the official release archive's pinned SHA-256, unpacks Charon
and Aeneas into the ignored `.tools/translator/`, installs Charon's nightly in WSL,
installs native Lean through elan, downloads Mathlib's cache, checks library pins,
and builds the proof project. Aeneas' Lean backend builds from source on Windows
because the release has no Windows Lean archive. Downloads and extracted tools stay in this worktree.
Elan/rustup installations and Mathlib's download cache are user-level caches.
The first setup downloads the external toolchains and library cache.

Keep the worktree on a Windows drive (including X:); WSL must be able to access
that drive. Paths pass through `wslpath` as arguments, including drive letters,
spaces, backslashes and long paths. The scripts do not interpolate paths into
shell commands. PowerShell propagates any failed native command as an error.

For an already initialized checkout, the mandatory local check is:

```powershell
Set-Location proofs
lake build
```

## Feature workflow

1. Model the feature in `<Feature>/Design.lean` and prove `correct`, `cost_bound`
   and `optimal`. Name the SPEC.md section, cost model and class of alternatives.
   Add its namespace to `roots` and `.submodules` to `globs` in `lakefile.lean`,
   import the proof from `Velocity.lean`, and run `lake build` before writing Rust.
2. Implement the pure synchronous Rust logic in the appropriate crate.
3. From the repository root, extract its library (the wrapper passes `--lib`):

   ```powershell
   powershell -File proofs/scripts/Export-Rust.ps1 -CratePath 'crates/velocity-core' -Feature Projects
   ```

   This runs these tools in order, with resolved WSL paths:

   ```text
   charon cargo --preset=aeneas --dest-file <proofs>/generated-llbc/Projects.llbc -- --lib
   aeneas -backend lean <llbc> -dest <proofs> -subdir Projects/Generated -split-files -namespace Velocity.Projects.Generated -no-progress-bar
   ```

4. Complete any generated external-definition templates. Never assume an
   unsupported function correct. Write `<Feature>/Confidence.lean`, importing
   the generated modules and proving their refinement of the design. Import it
   from `Velocity.lean`; `lake build` must include every generated module used in
   the proof. Keep generated Lean in the PR so reviewers can inspect it.
5. Add `#print axioms` for every public theorem. Run `lake build` before each
   push and inspect its axiom output. Only `propext`, `Classical.choice` and
   `Quot.sound` are allowed. No proof placeholders or `native_decide` are allowed.
   This bootstrap supplies the project and commands; automated audits and
   statement locking remain M11 work.

The native Windows `.lake` build uses Windows object files. Do not build that
same directory using Linux Lean in WSL. WSL runs only the translators here.

## Bootstrap proof scope

`Toolchain/Design.lean` models the successful Charon → Aeneas → Lean workflow.
Its cost is the number of stage invocations, excluding setup and each tool's
internal work. `optimal` quantifies over all finite stage sequences that begin
with Rust and reach checked Lean in that model.

`Toolchain/Confidence.lean` composes that model with the pinned Aeneas result type,
proves successful refinement and failure propagation, and requires both Mathlib
and Aeneas to compile in the native project. Its level is **model**. It does not
prove the PowerShell wrappers, external tools or filesystem. This PR adds no
application Rust, so there is no application translation or code-level claim.
Scouts and PR duty own adversarial verification of the wrappers.

## Updating pins

Choose an immutable Aeneas release first. Use its exact commit, its `charon-pin`,
its backend `lean-toolchain` and Mathlib manifest revision, and the bundled
`rust-toolchain`. Record the official release asset digest in
`toolchain-pins.json`; update `lakefile.lean` and `lean-toolchain` consistently.
Run `lake update` in this directory to regenerate the manifest, then setup and
`lake build`. Review every changed dependency before committing the manifest.

Upstream instructions: [Aeneas getting started](https://github.com/AeneasVerif/aeneas/blob/c5eb4a28cef65e0aa27743105308243e535a2a2b/documentation/getting-started.md)
and [Charon](https://github.com/AeneasVerif/charon/tree/8cae8fd1905946de61147a65ecdd7122cc6b491b).
