import { commands } from '../bindings';

// Open on Collapse (#429). Rust builds the page address from the project's
// Collapse mirror; the window passes only the project and, for a land, its commit.
export const collapseCommands = {
  open: (project: number, commit: string | null = null) =>
    commands.openOnCollapse(project, commit),
};
