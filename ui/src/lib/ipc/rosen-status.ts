import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  ROSEN_STATUS_EVENT,
  type RosenStatusChanged,
} from '../bindings';

export type { ChangeRow, OwningThread, RosenStatusChanged } from '../bindings';

export const watchRosenStatus = commands.watchRosenStatus;
export const onRosenStatus = (callback: EventCallback<RosenStatusChanged>) =>
  listen(ROSEN_STATUS_EVENT, callback);
