import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  ISSUE_PANEL_EVENT,
  type IssuePanelChanged_Serialize as IssuePanelChanged,
} from '../bindings';

export type {
  IssuePanelSnapshot_Serialize as IssuePanelSnapshot,
  IssuePanelRow_Serialize as IssuePanelRow,
  SpecQuestion,
} from '../bindings';

export const issueCommands = {
  panel: commands.getIssuePanel,
  open: commands.openProjectIssue,
  dispatch: commands.dispatchIssue,
};

export function onIssuePanel(callback: EventCallback<IssuePanelChanged>) {
  return listen<IssuePanelChanged>(ISSUE_PANEL_EVENT, callback);
}
