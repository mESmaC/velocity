import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  COLLAPSE_LINK_EVENT,
  commands,
  type CollapseActionResult,
  type CollapseConnection,
  type CollapseLinkOpened_Serialize,
  type CollapseRelayKind,
  type DeletedRepositoryAction,
  type IssueHost,
  type LayoutSnapshot_Serialize,
  type MirrorToCollapseResult_Serialize,
  type MoveToCollapseRequest_Deserialize,
  type MoveToCollapseResult_Serialize,
  type MoveToCollapseStepResult_Serialize,
  type ProjectCollapseSettings,
} from '../bindings';
export type {
  CollapseActionResult,
  CollapseCheckItemResult_Serialize as CollapseCheckItemResult,
  CollapseCheckItemStatus,
  CollapseCheckSummary_Serialize as CollapseCheckSummary,
  CollapseConnection,
  CollapseFeedHealth,
  CollapseLinkOpened_Serialize as CollapseLinkOpened,
  CollapseRelayKind,
  DeletedCollapseRepository_Serialize as DeletedCollapseRepository,
  DeletedRepositoryAction,
  GitHubIssueSyncView_Serialize as GitHubIssueSyncView,
  IssueHost,
  MirrorToCollapseResult_Serialize as MirrorToCollapseResult,
  MoveToCollapseRequest_Deserialize as MoveToCollapseRequest,
  MoveToCollapseResult_Serialize as MoveToCollapseResult,
  MoveToCollapseStepId,
  MoveToCollapseStepResult_Serialize as MoveToCollapseStepResult,
  MoveToCollapseStepStatus,
  ProjectCollapseSettings,
  PushTokenStatus,
} from '../bindings';

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
// Project Collapse settings (#576): inspect feed health and push token status, toggle
// `collapse_channel`, set allowed relay actions, and unlink the `collapse` root.
export const collapseCommands = {
  open: (project: number, commit: string | null = null) =>
    commands.openOnCollapse(project, commit),
  openPage: (url: string): Promise<null> => commands.openCollapsePage(url),
  mirror: (
    project: number,
    deletedAction: DeletedRepositoryAction | null = null,
  ): Promise<MirrorToCollapseResult_Serialize> =>
    commands.mirrorToCollapse(project, deletedAction),
  sendLiveState: (project: number): Promise<null> =>
    commands.sendCollapseLiveState(project),
  projectSettings: (project: number): Promise<ProjectCollapseSettings | null> =>
    commands.getProjectCollapseSettings(project),
  setChannel: (
    project: number,
    enabled: boolean,
  ): Promise<CollapseActionResult> =>
    commands.setCollapseChannel(project, enabled),
  setRelay: (
    project: number,
    allow: CollapseRelayKind[],
  ): Promise<ProjectCollapseSettings> =>
    commands.setCollapseRelay(project, allow),
  setIssueHost: (
    project: number,
    host: IssueHost,
  ): Promise<ProjectCollapseSettings> =>
    commands.setProjectIssueHost(project, host),
  setIssuesToken: (
    project: number,
    token: string,
  ): Promise<ProjectCollapseSettings> =>
    commands.setCollapseIssuesToken(project, token),
  clearIssuesToken: (project: number): Promise<ProjectCollapseSettings> =>
    commands.clearCollapseIssuesToken(project),
  unlink: (project: number): Promise<CollapseActionResult> =>
    commands.unlinkCollapse(project),
  moveToCollapse: (
    request: MoveToCollapseRequest_Deserialize,
  ): Promise<MoveToCollapseResult_Serialize> =>
    commands.moveToCollapse(request),
  importIssues: (
    project: number,
  ): Promise<MoveToCollapseStepResult_Serialize> =>
    commands.importCollapseIssues(project),
  setGithubIssueSync: (
    project: number,
    enabled: boolean,
  ): Promise<ProjectCollapseSettings> =>
    commands.setGithubIssueSync(project, enabled),
  syncGithubIssuesNow: (project: number): Promise<ProjectCollapseSettings> =>
    commands.syncGithubIssuesNow(project),
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

// Open in Velocity (#479): Rust parses `velocity://collapse/...` links and emits
// either the matched project with optional draft context or an unmatched notice.
export const onCollapseLink = (
  callback: EventCallback<CollapseLinkOpened_Serialize>,
) => listen(COLLAPSE_LINK_EVENT, callback);
