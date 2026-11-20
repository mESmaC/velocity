import type {
  SubagentStatus,
  SubagentView,
  ToolStepRow,
  TranscriptBlock,
} from './ipc';

export type ToolGroupSegment =
  | { kind: 'tools'; steps: ToolStepRow[] }
  | { kind: 'subagents'; subagents: SubagentView[] };

/** Formats a subagent's elapsed wall time from `started_at` to `ended_at` or `nowSeconds`. */
export function formatSubagentDuration(
  subagent: SubagentView,
  nowSeconds: number,
): string {
  const start = Number(subagent.started_at);
  if (!Number.isFinite(start) || start <= 0) return '0s';
  const end =
    subagent.status === 'Running'
      ? nowSeconds
      : Number(subagent.ended_at ?? nowSeconds);
  const seconds = Math.max(0, Math.floor(end - start));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;
  if (minutes < 60) return `${minutes}m ${remainingSeconds}s`;
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return `${hours}h ${remainingMinutes}m`;
}

export function formatSubagentSteps(stepCount: string): string {
  const count = Number(stepCount);
  if (!Number.isFinite(count) || count < 0) return '0 steps';
  return count === 1 ? '1 step' : `${count} steps`;
}

export function formatSubagentTokens(tokens: string | null): string | null {
  if (!tokens) return null;
  const count = Number(tokens);
  if (!Number.isFinite(count) || count <= 0) return null;
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M tokens`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}k tokens`;
  return `${count} tokens`;
}

export function formatSubagentStatus(status: SubagentStatus): string {
  if (status === 'Running') return 'running';
  if (status === 'Failed') return 'failed';
  return 'done';
}

export function formatSubagentMeta(
  subagent: SubagentView,
  defaultModel: string,
  nowSeconds: number,
): string {
  const parts: string[] = [];
  const model = subagent.model?.trim() || defaultModel.trim();
  if (model) parts.push(model);
  parts.push(formatSubagentDuration(subagent, nowSeconds));
  parts.push(formatSubagentSteps(subagent.step_count));
  const tokens = formatSubagentTokens(subagent.tokens);
  if (tokens) parts.push(tokens);
  return parts.join(' · ');
}

export function formatRunningSubagents(
  count: number,
  style: 'running' | 'short' = 'running',
): string {
  const noun = count === 1 ? 'subagent' : 'subagents';
  return style === 'running' ? `${count} ${noun} running` : `${count} ${noun}`;
}

/**
 * Splits a `ToolGroup` into regular tool-step runs and stacked subagent card runs
 * so each spawning tool step becomes a live subagent card in place (F4).
 */
export function partitionToolGroup(
  steps: ToolStepRow[],
  subagentById: ReadonlyMap<string, SubagentView>,
): ToolGroupSegment[] {
  const segments: ToolGroupSegment[] = [];
  for (const step of steps) {
    const matched = step.tool_call_id
      ? subagentById.get(step.tool_call_id)
      : undefined;
    if (matched) {
      const last = segments.at(-1);
      if (last?.kind === 'subagents') {
        last.subagents.push(matched);
      } else {
        segments.push({ kind: 'subagents', subagents: [matched] });
      }
    } else {
      const last = segments.at(-1);
      if (last?.kind === 'tools') {
        last.steps.push(step);
      } else {
        segments.push({ kind: 'tools', steps: [step] });
      }
    }
  }
  return segments;
}

/** Returns subagents whose identifier has no spawning tool step in `blocks`. */
export function findUnmatchedSubagents(
  blocks: TranscriptBlock[],
  subagents: SubagentView[],
): SubagentView[] {
  if (subagents.length === 0) return [];
  const byId = new Set(subagents.map((subagent) => subagent.identifier));
  const matched = new Set<string>();
  for (const block of blocks) {
    if (!block.ToolGroup) continue;
    for (const step of block.ToolGroup.steps) {
      if (step.tool_call_id && byId.has(step.tool_call_id)) {
        matched.add(step.tool_call_id);
      }
    }
  }
  return subagents.filter((subagent) => !matched.has(subagent.identifier));
}

/** Replaces an existing subagent entry by identifier, or appends it in order. */
export function mergeSubagentUpdate(
  list: SubagentView[],
  updated: SubagentView,
): SubagentView[] {
  const index = list.findIndex(
    (entry) => entry.identifier === updated.identifier,
  );
  if (index === -1) return [...list, updated];
  const next = list.slice();
  next[index] = updated;
  return next;
}
