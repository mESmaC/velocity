import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  LOCAL_BUILD_STATUS_EVENT,
  type LocalBuildStatusChanged,
} from '../bindings';

export type {
  LocalBuildConfirm,
  LocalBuildOffer,
  LocalBuildStatusChanged,
  LocalBuildView,
} from '../bindings';

// Velocity on its own landed fixes (SPEC.md F1 "Local builds"): the title bar's restart offers.
export const localBuildCommands = {
  status: commands.getLocalBuildStatus,
  restart: commands.restartIntoLocalBuild,
  cancelRestart: commands.cancelLocalBuildRestart,
  setAutoRestart: commands.setLocalBuildAutoRestart,
};

export const onLocalBuildStatus = (
  callback: EventCallback<LocalBuildStatusChanged>,
) => listen(LOCAL_BUILD_STATUS_EVENT, callback);
