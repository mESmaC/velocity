import type {
  LayoutDelta,
  LayoutSnapshot,
  ThreadDetailMeta,
  ThreadDetailSnapshot,
  ThreadTranscriptChanged,
  TranscriptBlock,
  TranscriptPatch,
  TranscriptUpdate,
} from './ipc';

/** Monotonic block identity generator so keyed rendering survives virtualization and tail edits. */
let nextBlockId = 1;

function allocateId(): number {
  const id = nextBlockId;
  nextBlockId += 1;
  return id;
}

function blockVariant(block: TranscriptBlock): string {
  if (block.UserMessage) return 'UserMessage';
  if (block.AgentMessage) return 'AgentMessage';
  if (block.ToolGroup) return 'ToolGroup';
  if (block.Diff) return 'Diff';
  if (block.StatusLine) return 'StatusLine';
  if (block.ErrorCard) return 'ErrorCard';
  if (block.Decision) return 'Decision';
  if (block.Notice) return 'Notice';
  return 'Unknown';
}

export interface KeyedTranscript {
  blocks: TranscriptBlock[];
  ids: number[];
  revision: number;
}

/** Creates a freshly keyed transcript from a full snapshot block list. */
export function resetKeyedTranscript(
  blocks: TranscriptBlock[],
  revision: number,
): KeyedTranscript {
  const ids = new Array<number>(blocks.length);
  for (let index = 0; index < blocks.length; index += 1) {
    ids[index] = allocateId();
  }
  return { blocks, ids, revision };
}

/**
 * Applies ordered transcript patches in place onto shallow-copied arrays so untouched
 * blocks keep their exact object references and stable IDs without deep-comparing (#553).
 * Returns `null` if any patch index does not match (caller should resync).
 */
export function applyTranscriptPatches(
  current: KeyedTranscript,
  patches: TranscriptPatch[],
  nextRevision: number,
): KeyedTranscript | null {
  if (patches.length === 0) {
    return current.revision === nextRevision
      ? current
      : { blocks: current.blocks, ids: current.ids, revision: nextRevision };
  }
  const blocks = current.blocks.slice();
  const ids = current.ids.slice();

  for (const patch of patches) {
    if (patch.AppendAgentText) {
      const index = Number(patch.AppendAgentText.index);
      const existing = blocks[index];
      if (!existing?.AgentMessage) return null;
      const previousParagraphs = existing.AgentMessage.paragraphs;
      const nextParagraphs =
        previousParagraphs.length === 0
          ? [patch.AppendAgentText.delta]
          : [
              ...previousParagraphs.slice(0, -1),
              previousParagraphs[previousParagraphs.length - 1] +
                patch.AppendAgentText.delta,
            ];
      blocks[index] = { AgentMessage: { paragraphs: nextParagraphs } };
    } else if (patch.UpdateBlock) {
      const index = Number(patch.UpdateBlock.index);
      if (index < 0 || index >= blocks.length) return null;
      const previous = blocks[index];
      blocks[index] = patch.UpdateBlock.block;
      if (blockVariant(previous) !== blockVariant(patch.UpdateBlock.block)) {
        ids[index] = allocateId();
      }
    } else if (patch.ReplaceTail) {
      const start = Number(patch.ReplaceTail.start);
      if (start < 0 || start > blocks.length) return null;
      const tail = patch.ReplaceTail.blocks;
      const previousTail = blocks.slice(start);
      const previousIds = ids.slice(start);
      blocks.length = start;
      ids.length = start;
      for (let offset = 0; offset < tail.length; offset += 1) {
        const nextBlock = tail[offset];
        const prior = previousTail[offset];
        blocks.push(nextBlock);
        ids.push(
          prior && blockVariant(prior) === blockVariant(nextBlock)
            ? previousIds[offset]
            : allocateId(),
        );
      }
    }
  }

  return { blocks, ids, revision: nextRevision };
}

function mergeThreadMeta(
  meta: ThreadDetailMeta,
  blocks: TranscriptBlock[],
  transcriptRevision: number,
): ThreadDetailSnapshot {
  return {
    ...meta,
    transcript_revision: transcriptRevision,
    blocks,
  };
}

export type DeltaOutcome =
  | { kind: 'applied'; snapshot: LayoutSnapshot; transcript: KeyedTranscript }
  | { kind: 'ignored' }
  | { kind: 'resync' };

/**
 * Applies a [`LayoutDelta`] onto the current [`LayoutSnapshot`] and [`KeyedTranscript`],
 * keeping references for every omitted slice intact without generic deep comparison (#553).
 */
export function applyLayoutDelta(
  current: LayoutSnapshot | null,
  currentTranscript: KeyedTranscript,
  delta: LayoutDelta,
): DeltaOutcome {
  if (!current) return { kind: 'resync' };
  if (delta.revision <= current.revision) return { kind: 'ignored' };
  if (delta.base_revision !== current.revision) return { kind: 'resync' };

  const next: LayoutSnapshot = {
    ...current,
    revision: delta.revision,
  };

  if (delta.chrome) {
    Object.assign(next, delta.chrome);
  }
  if (delta.sidebar) {
    next.groups = delta.sidebar.groups;
    next.pulse = delta.sidebar.pulse;
  }
  if (delta.board !== undefined && delta.board !== null) {
    next.board = delta.board;
  }
  if (delta.rail) {
    next.tasks = delta.rail.tasks;
    next.pull_requests = delta.rail.pull_requests;
    next.issues = delta.rail.issues;
    next.tasks_draft = delta.rail.tasks_draft;
    next.task_count_label = delta.rail.task_count_label;
    next.rosen_work = delta.rail.rosen_work;
  }
  if (delta.palette) {
    next.palette_open = delta.palette.palette_open;
    next.palette_query = delta.palette.palette_query;
    next.palette_results = delta.palette.palette_results;
    next.palette_selection = delta.palette.palette_selection;
  }

  let nextTranscript = currentTranscript;
  if (delta.active_thread_cleared) {
    next.active_thread = null;
    nextTranscript = resetKeyedTranscript([], 0);
  } else if (delta.active_thread_meta || delta.active_thread_blocks) {
    const updatedTranscript = applyTranscriptUpdate(
      currentTranscript,
      delta.active_thread_blocks ?? null,
    );
    if (!updatedTranscript) return { kind: 'resync' };
    nextTranscript = updatedTranscript;

    if (delta.active_thread_meta) {
      next.active_thread = mergeThreadMeta(
        delta.active_thread_meta,
        nextTranscript.blocks,
        nextTranscript.revision,
      );
    } else if (current.active_thread) {
      next.active_thread = {
        ...current.active_thread,
        transcript_revision: nextTranscript.revision,
        blocks: nextTranscript.blocks,
      };
    } else {
      return { kind: 'resync' };
    }
  }

  if (delta.active_duty_cleared) {
    next.active_duty = null;
  } else if (delta.active_duty !== undefined && delta.active_duty !== null) {
    next.active_duty = delta.active_duty;
  }

  if (delta.active_swarm_cleared) {
    next.active_swarm = null;
  } else if (delta.active_swarm !== undefined && delta.active_swarm !== null) {
    next.active_swarm = delta.active_swarm;
  }

  if (delta.active_goal_swarm_cleared) {
    next.active_goal_swarm = null;
  } else if (
    delta.active_goal_swarm !== undefined &&
    delta.active_goal_swarm !== null
  ) {
    next.active_goal_swarm = delta.active_goal_swarm;
  }

  return { kind: 'applied', snapshot: next, transcript: nextTranscript };
}

function applyTranscriptUpdate(
  current: KeyedTranscript,
  update: TranscriptUpdate | null,
): KeyedTranscript | null {
  if (!update) return current;
  if (update.Full) {
    return resetKeyedTranscript(update.Full.blocks, update.Full.revision);
  }
  if (!update.Patches) return null;
  const { base_revision, revision, patches } = update.Patches;
  if (revision <= current.revision) return current;
  if (base_revision !== current.revision) return null;
  return applyTranscriptPatches(current, patches, revision);
}

/**
 * Applies a thread-scoped `thread://transcript` delta directly to the open thread
 * without rebuilding or touching any other part of `LayoutSnapshot` (#553).
 */
export function applyThreadTranscript(
  current: LayoutSnapshot | null,
  currentTranscript: KeyedTranscript,
  event: ThreadTranscriptChanged,
): DeltaOutcome {
  if (!current || !current.active_thread) return { kind: 'ignored' };
  if (
    current.project_identifier !== event.project_identifier ||
    current.active_thread.identifier !== event.thread_identifier
  ) {
    return { kind: 'ignored' };
  }
  if (event.transcript_revision <= currentTranscript.revision) {
    return { kind: 'ignored' };
  }
  if (event.base_transcript_revision !== currentTranscript.revision) {
    return { kind: 'resync' };
  }

  const nextTranscript = applyTranscriptPatches(
    currentTranscript,
    event.patches,
    event.transcript_revision,
  );
  if (!nextTranscript) return { kind: 'resync' };

  const active = current.active_thread;
  const nextActive: ThreadDetailSnapshot = {
    ...active,
    transcript_revision: nextTranscript.revision,
    blocks: nextTranscript.blocks,
    added:
      event.added !== undefined && event.added !== null
        ? String(event.added)
        : active.added,
    deleted:
      event.deleted !== undefined && event.deleted !== null
        ? String(event.deleted)
        : active.deleted,
    agent_busy:
      event.agent_busy !== undefined && event.agent_busy !== null
        ? event.agent_busy
        : active.agent_busy,
    titling:
      event.titling !== undefined && event.titling !== null
        ? event.titling[0]
        : active.titling,
    title_model_name:
      event.titling !== undefined && event.titling !== null
        ? event.titling[1]
        : active.title_model_name,
  };

  return {
    kind: 'applied',
    snapshot: { ...current, active_thread: nextActive },
    transcript: nextTranscript,
  };
}
