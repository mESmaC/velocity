import { listen, type EventCallback } from '@tauri-apps/api/event';
import { commands, DUTY_EVENT, type DutyEventPublished } from '../bindings';

export type {
  DismissDutyAgentRequest,
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
  StopDutyRequest,
} from '../bindings';

// Duty view, Start a duty agent dialog, pause/resume, add, dismiss and stop (SPEC.md F7).
export const dutyCommands = {
  defaults: commands.getDutyDialogDefaults,
  start: commands.startDuty,
  pause: commands.pauseDuty,
  addAgents: commands.addDutyAgents,
  dismissAgent: commands.dismissDutyAgent,
  stop: commands.stopDuty,
};

export const onDutyEvent = (callback: EventCallback<DutyEventPublished>) =>
  listen(DUTY_EVENT, callback);
