import { listen, type EventCallback } from '@tauri-apps/api/event';
export * from './ipc/agents';
export * from './ipc/goal-swarm';
export * from './ipc/issues';
export * from './ipc/skills';
export * from './ipc/swarm';
import {
  commands,
  WINDOW_STATE_EVENT,
  LAYOUT_STATE_EVENT,
  LAYOUT_INTENT_EVENT,
  PROJECT_SELECTED_EVENT,
  PULL_REQUEST_PANEL_EVENT,
  THREAD_STATUS_EVENT,
  THREAD_STEP_EVENT,
  type ThreadStatusChanged,
  type ThreadStepRecorded_Serialize,
  type WindowStateChanged,
  type LayoutStateChanged_Serialize,
  type LayoutIntentRequested_Serialize,
  type ProjectSelected,
  WORKTREE_INVENTORY_EVENT,
  type WorktreeInventoryChanged_Serialize,
  type PullRequestPanelChanged,
} from './bindings';

export type {
  AgentActivity,
  AgentSettings,
  DiffBlockView_Serialize as DiffBlockView,
  DiffLineRole,
  DiffLineView,
  EffortLevel,
  ErrorCardView,
  InterviewView_Serialize as InterviewView,
  ModelPillView,
  LaunchEligibility,
  LaunchRequest,
  PersistedState,
  ProjectItem,
  ProjectLocationKind,
  ProofSettings,
  ProviderColor,
  ProviderKind,
  ProviderSettings,
  ReviewStage,
  Settings,
  SidebarGroup_Serialize as SidebarGroup,
  SidebarRow_Serialize as SidebarRow,
  AgentCommandView,
  SlashCommandItem,
  SpeedMode,
  TaskAttribution,
  TaskItem,
  TaskState,
  ThreadDetailSnapshot_Serialize as ThreadDetailSnapshot,
  ThreadItem,
  ThreadState,
  ThreadStatusChanged,
  ThreadStepRecorded_Serialize as ThreadStepRecorded,
  ToolStepRow,
  TranscriptBlock_Serialize as TranscriptBlock,
  TranscriptModeView,
  WindowStateChanged,
  WorkedSummaryView_Serialize as WorkedSummaryView,
  WorktreeChipView,
  WorktreeSettings,
  LayoutAction_Deserialize as LayoutAction,
  LayoutSnapshot_Serialize as LayoutSnapshot,
  LayoutIntent_Serialize as LayoutIntent,
  NavigationIntent_Deserialize as NavigationIntent,
  NewProjectDefaults_Serialize as NewProjectDefaults,
  NewProjectRequest_Deserialize as NewProjectRequest,
  ProjectLocationRequest,
  RepositoryVisibility,
  RailItem_Serialize as RailItem,
  ProjectList,
  ProjectRow,
  ProjectLabel,
  WorktreeSettingsView_Serialize as WorktreeSettingsView,
  WorktreeSettingsEdit,
  WorktreePathPreview,
  PullRequest,
  PullRequestSnapshot,
  PullRequestCheck,
  PullRequestReview,
  PullRequestComment,
  PullRequestReviewThread,
  PullRequestOperation,
  PullRequestReceipt,
  CreatePullRequest,
  ReviewAction,
  ReviewSide,
  NewReviewComment,
  MergeMethod,
  PullRequestPanelRow,
  PullRequestPanelSnapshot,
} from './bindings';

export const pullRequestCommands = {
  list: commands.listPullRequests,
  get: commands.getPullRequest,
  dispatch: commands.dispatchPullRequest,
  panel: commands.getPullRequestPanel,
  open: commands.openProjectPullRequest,
};

export const onPullRequestPanel = (
  callback: EventCallback<PullRequestPanelChanged>,
) => listen(PULL_REQUEST_PANEL_EVENT, callback);

export const layoutCommands = {
  getState: commands.getLayoutState,
  dispatch: commands.dispatchLayout,
};

export const projectCommands = {
  list: commands.listProjects,
  switch: commands.switchProject,
};

export const onProjectSelected = (callback: EventCallback<ProjectSelected>) =>
  listen(PROJECT_SELECTED_EVENT, callback);

export const onLayoutState = (
  callback: EventCallback<LayoutStateChanged_Serialize>,
) => listen(LAYOUT_STATE_EVENT, callback);

export const onLayoutIntent = (
  callback: EventCallback<LayoutIntentRequested_Serialize>,
) => listen(LAYOUT_INTENT_EVENT, callback);

export const newProjectCommands = {
  defaults: commands.getNewProjectDefaults,
  pickFolder: commands.pickProjectFolder,
  create: commands.createProject,
};

export const windowCommands = {
  getState: commands.getWindowState,
  minimize: commands.minimizeWindow,
  toggleMaximize: commands.toggleMaximizeWindow,
  close: commands.closeWindow,
};

export const settingsCommands = {
  get: commands.getSettings,
  set: commands.setSettings,
};

export const worktreeCommands = {
  list: commands.listWorktrees,
  set: commands.setWorktreeSettings,
  preview: commands.previewWorktreeFolder,
  browse: commands.browseWorktreeFolder,
  prune: commands.pruneWorktrees,
};

export const onWorktreeInventory = (
  callback: EventCallback<WorktreeInventoryChanged_Serialize>,
) => listen(WORKTREE_INVENTORY_EVENT, callback);

export const launchCommands = {
  getEligibility: commands.getLaunchEligibility,
};

export const onWindowState = (callback: EventCallback<WindowStateChanged>) =>
  listen(WINDOW_STATE_EVENT, callback);

export * from './ipc/follow-through';
// Threads start and steer through the composer's layout intents (`start_thread`,
// `send_message`); their lifecycle also goes out as these events (SPEC.md §3.1).
export const onThreadStatus = (callback: EventCallback<ThreadStatusChanged>) =>
  listen(THREAD_STATUS_EVENT, callback);

export const onThreadStep = (
  callback: EventCallback<ThreadStepRecorded_Serialize>,
) => listen(THREAD_STEP_EVENT, callback);

export * from './ipc/duty';
export * from './ipc/thread-menu';

export * from './ipc/airc';
export * from './ipc/airc-events';
export * from './ipc/work-pool';
