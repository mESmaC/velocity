import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  AIRC_EVENT,
  AIRC_MESSAGE_EVENT,
  type AircEventPublished_Serialize,
  type AircMessagePublished_Serialize,
} from '../bindings';

export type {
  AircEventPublished_Serialize as AircEventPublished,
  AircMessagePublished_Serialize as AircMessagePublished,
  Entry_Serialize as AircEntry,
} from '../bindings';

export const aircEventCommands = {
  events: commands.aircEvents,
  messages: commands.aircMessages,
  ledger: commands.aircLedger,
};

export const onAircEvent = (
  callback: EventCallback<AircEventPublished_Serialize>,
) => listen(AIRC_EVENT, callback);

export const onAircMessage = (
  callback: EventCallback<AircMessagePublished_Serialize>,
) => listen(AIRC_MESSAGE_EVENT, callback);
