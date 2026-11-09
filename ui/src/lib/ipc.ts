import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  WINDOW_STATE_EVENT,
  type AgentActivity,
  type AgentSettings,
  type EffortLevel,
  type PersistedState,
  type ProjectItem,
  type ProjectLocationKind,
  type ProofSettings,
  type ProviderKind,
  type ProviderSettings,
  type ReviewStage,
  type Settings,
  type SpeedMode,
  type TaskAttribution,
  type TaskItem,
  type TaskState,
  type ThreadItem,
  type ThreadState,
  type WindowStateChanged,
  type WorktreeSettings,
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

export const onWindowState = (callback: EventCallback<WindowStateChanged>) =>
  listen(WINDOW_STATE_EVENT, callback);
