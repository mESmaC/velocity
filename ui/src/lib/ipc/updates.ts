import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  UPDATE_STATUS_EVENT,
  type UpdateStatusChanged,
} from '../bindings';

export type {
  UpdateAction,
  UpdateStatusChanged,
  UpdateView,
} from '../bindings';

// Velocity's own updates (SPEC.md F1 "Updates"): the footer badge and panel.
export const updateCommands = {
  status: commands.getUpdateStatus,
  check: commands.checkForUpdate,
  install: commands.installUpdate,
};

export const onUpdateStatus = (callback: EventCallback<UpdateStatusChanged>) =>
  listen(UPDATE_STATUS_EVENT, callback);
