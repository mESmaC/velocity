import { listen, type EventCallback } from '@tauri-apps/api/event';
import {
  THREAD_SUBAGENT_EVENT,
  type ThreadSubagentChanged_Serialize,
} from '../bindings';

// A thread's subagents (SPEC.md §3.2 Subagents, §3.1 F4): the open thread's snapshot lists
// them in `subagents`, and each change also goes out as `thread://subagent`.
export type {
  SubagentStatus,
  SubagentView_Serialize as SubagentView,
  ThreadSubagentChanged_Serialize as ThreadSubagentChanged,
} from '../bindings';

export const onThreadSubagent = (
  callback: EventCallback<ThreadSubagentChanged_Serialize>,
) => listen(THREAD_SUBAGENT_EVENT, callback);
