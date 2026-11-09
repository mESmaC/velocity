import { listen, type EventCallback } from '@tauri-apps/api/event';
import { commands, DUTY_EVENT, type DutyEventPublished } from '../bindings';

export type {
  DutyAgentCard,
  DutyDialogDefaults,
  DutyEventKind,
  DutyEventPublished,
  DutyFeedEvent,
  DutyMergeMethod,
  DutyRole,
  DutyRuleChip,
  DutyViewSnapshot_Serialize as DutyViewSnapshot,
  PauseDutyRequest,
  PrDutyRuleSettings,
  ScoutCadence,
  ScoutFocusArea,
  ScoutRuleSettings,
  StartDutyRequest_Deserialize as StartDutyRequest,
} from '../bindings';

// Duty view, Start a duty agent dialog, pause/resume and add agents (SPEC.md F7).
export const dutyCommands = {
  defaults: commands.getDutyDialogDefaults,
  start: commands.startDuty,
  pause: commands.pauseDuty,
  addAgents: commands.addDutyAgents,
};

export const onDutyEvent = (callback: EventCallback<DutyEventPublished>) =>
  listen(DUTY_EVENT, callback);
