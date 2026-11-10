import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  AIRC_MODAL_EVENT,
  type AircModalPublished,
} from '../bindings';

export type {
  AircClaimRow,
  AircLineView,
  AircModalPublished,
  AircModalView,
  AircNickGroup,
  AircNickRow,
  AircSpan,
  AircTab,
  AircTabView,
  AircTone,
  NickDot,
  NickTone,
} from '../bindings';

export const aircModalCommands = { view: commands.getAircModal };

export const onAircModal = (callback: EventCallback<AircModalPublished>) =>
  listen(AIRC_MODAL_EVENT, callback);
