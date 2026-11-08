import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  WINDOW_STATE_EVENT,
  type WindowStateChanged,
} from './bindings';

export type { WindowStateChanged } from './bindings';

export const windowCommands = {
  getState: commands.getWindowState,
  minimize: commands.minimizeWindow,
  toggleMaximize: commands.toggleMaximizeWindow,
  close: commands.closeWindow,
};

export const onWindowState = (callback: EventCallback<WindowStateChanged>) =>
  listen(WINDOW_STATE_EVENT, callback);
