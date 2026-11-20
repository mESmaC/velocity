import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  ROSEN_STATUS_EVENT,
  type LayoutSnapshot_Serialize,
  type RosenStatusChanged,
} from '../bindings';

export type { ChangeRow, OwningThread, RosenStatusChanged } from '../bindings';

export const watchRosenStatus = commands.watchRosenStatus;
export const switchToAfterLand = commands.switchToAfterLand;
export const onRosenStatus = (callback: EventCallback<RosenStatusChanged>) =>
  listen(ROSEN_STATUS_EVENT, callback);

// GitHub CI on the project's mirrored base, as Rust last read it (#507). The Rosen
// status pane shows it beside the base's last green integration commit.
export type BaseCi = NonNullable<LayoutSnapshot_Serialize['base_ci']>;
