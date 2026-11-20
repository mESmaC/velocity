import {
  commands,
  type CollapseConnection,
  type LayoutSnapshot_Serialize,
} from '../bindings';
export type { CollapseConnection } from '../bindings';

// What Collapse last reported for the project's mirror: its last land and the
// forwarder to the downstream remote (#427). Rust reads it; the window shows it.
export type CollapseMirrorHealth = NonNullable<
  LayoutSnapshot_Serialize['collapse_mirror_health']
>;

// Open on Collapse (#429). Rust builds the page address from the project's
// Collapse mirror; the window passes only the project and, for a land, its commit.
// Mirror to Collapse (#427): Rust creates the repository, hands its push token to
// git's credential helper and lands the rosen.toml change; a toast says how it went.
// Send Collapse its live state (#517): for a project already mirrored there, Rust gives
// Rosen's state feed the push token and lands the rosen.toml `collapse` root.
export const collapseCommands = {
  open: (project: number, commit: string | null = null) =>
    commands.openOnCollapse(project, commit),
  mirror: (project: number): Promise<null> =>
    commands.mirrorToCollapse(project),
  sendLiveState: (project: number): Promise<null> =>
    commands.sendCollapseLiveState(project),
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
