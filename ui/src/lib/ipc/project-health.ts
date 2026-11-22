import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  commands,
  PROJECT_HEALTH_EVENT,
  type ProjectHealthChanged,
} from '../bindings';

export type {
  BlockedDispatchIssue,
  BlockedIssueKind,
  DispatchIdleAlert,
  DispatchIdleIssueAction,
  HealthSeverity,
  HealthSignalKind,
  HostIssueAction,
  HostSwitchBack,
  ProjectHealthChanged,
  ProjectHealthHistoryEntry,
  ProjectHealthItem,
  ProjectHealthView,
} from '../bindings';

// Per-project health checks and automatic remediation summaries (SPEC.md F5, #509, #616).
export const projectHealthCommands = {
  get: commands.getProjectHealth,
  actOnDispatchIdleIssue: commands.actOnDispatchIdleIssue,
  resolveHostChange: commands.resolveProjectHostChange,
  copyGithubIssueToCollapse: commands.copyGithubIssueToCollapse,
  dismissHostIssueProblem: commands.dismissHostIssueProblem,
};

export const onProjectHealth = (
  callback: EventCallback<ProjectHealthChanged>,
) => listen(PROJECT_HEALTH_EVENT, callback);
