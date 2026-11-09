import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  WINDOW_STATE_EVENT,
  LAYOUT_STATE_EVENT,
  LAYOUT_INTENT_EVENT,
  type WindowStateChanged,
  type LayoutStateChanged_Serialize,
  type LayoutIntentRequested_Serialize,
} from './bindings';

export type {
  AgentActivity,
  AgentSettings,
  EffortLevel,
  PersistedState,
  ProjectItem,
  ProjectLocationKind,
  ProofSettings,
  ProviderKind,
  ProviderSettings,
  ReviewStage,
  Settings,
  SpeedMode,
  TaskAttribution,
  TaskItem,
  TaskState,
  ThreadItem,
  ThreadState,
  WindowStateChanged,
  WorktreeSettings,
  LayoutAction_Deserialize as LayoutAction,
  LayoutSnapshot_Serialize as LayoutSnapshot,
  LayoutIntent_Serialize as LayoutIntent,
  NavigationIntent_Deserialize as NavigationIntent,
  RailItem_Serialize as RailItem,
} from './bindings';

export const layoutCommands = {
  getState: commands.getLayoutState,
  dispatch: commands.dispatchLayout,
};

export const onLayoutState = (
  callback: EventCallback<LayoutStateChanged_Serialize>,
) => listen(LAYOUT_STATE_EVENT, callback);

export const onLayoutIntent = (
  callback: EventCallback<LayoutIntentRequested_Serialize>,
) => listen(LAYOUT_INTENT_EVENT, callback);

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

export const onWindowState = (callback: EventCallback<WindowStateChanged>) =>
  listen(WINDOW_STATE_EVENT, callback);
