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
  IssueDiscussion,
  TrackingProgress,
  VisibleHold,
} from '../bindings';

export const issueCommands = {
  panel: commands.getIssuePanel,
  open: commands.openProjectIssue,
  dispatch: commands.dispatchIssue,
  dispatchWork: commands.dispatchWorkIssue,
  releaseHold: commands.releaseIssueHold,
  toggleExpanded: commands.toggleIssueExpanded,
  discussion: commands.getIssueDiscussion,
  openDiscussion: commands.openIssueDiscussion,
  setWorkExpanded: commands.setWorkIssueExpanded,
};

export function onIssuePanel(callback: EventCallback<IssuePanelChanged>) {
  return listen<IssuePanelChanged>(ISSUE_PANEL_EVENT, callback);
}
