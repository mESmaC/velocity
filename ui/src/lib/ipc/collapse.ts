import { commands, type CollapseConnection } from '../bindings';
export type { CollapseConnection } from '../bindings';

// Open on Collapse (#429). Rust builds the page address from the project's
// Collapse mirror; the window passes only the project and, for a land, its commit.
export const collapseCommands = {
  open: (project: number, commit: string | null = null) =>
    commands.openOnCollapse(project, commit),
};

// Collapse's read-only MCP server (#430). The tokens stay in the OS credential
// store; the window sees only the connected account and the switch.
export const collapseConnectionCommands = {
  read: (): Promise<CollapseConnection> => commands.getCollapseConnection(),
  connect: (): Promise<CollapseConnection> => commands.connectCollapse(),
  disconnect: (): Promise<CollapseConnection> => commands.disconnectCollapse(),
  setEnabled: (enabled: boolean): Promise<CollapseConnection> =>
    commands.setCollapseMcp(enabled),
};
