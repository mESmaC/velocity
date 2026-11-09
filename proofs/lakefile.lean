import Lake
open Lake DSL

require mathlib from git
  "https://github.com/leanprover-community/mathlib4.git" @
    "56a005855ea17e0b0958f73ec801ae140b0a1679"

require aeneas from git
  "https://github.com/AeneasVerif/aeneas.git" @
    "c5eb4a28cef65e0aa27743105308243e535a2a2b" / "backends/lean"

package velocity where
  moreLeanArgs := #["-DwarningAsError=true"]

@[default_target]
lean_lib Velocity where
  roots := #[`Velocity, `Toolchain, `Shell, `Theme, `AgentRunner, `State, `Layout, `WorktreeManager,
    `AcpClient]
  globs := #[.one `Velocity, .submodules `Toolchain, .submodules `Shell, .submodules `Theme,
    .submodules `AgentRunner, .submodules `State, .submodules `Layout, .submodules `WorktreeManager,
    .submodules `AcpClient]

lean_exe shell_appearance_reference where
  root := `Shell.ReferenceMain