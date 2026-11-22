import type {
  IssuePanelSnapshot,
  IssuePanelRow,
  LayoutSnapshot,
  WorkItem,
} from './ipc';

export type WorkTask = {
  kind: 'task';
  key: string;
  task: LayoutSnapshot['tasks'][number];
  item?: WorkItem;
};

export type WorkIssue = {
  kind: 'issue';
  key: string;
  repository: string;
  number: number;
  title: string;
  row?: IssuePanelRow;
  item?: WorkItem;
};

type WorkRow = WorkTask | WorkIssue;

const states = {
  claimed: 'Claimed',
  in_change: 'In change',
  landed: 'Landed',
  green: 'Green',
  reopened: 'Reopened',
};

export function workState(item: WorkItem | undefined, fallback: string) {
  return item ? states[item.status] : fallback;
}

export function issueKey(repository: string, number: number) {
  return `${repository.toLowerCase()}#${number}`;
}

export function completion(row: WorkRow): string | number | null | undefined {
  if (row.item?.status === 'green') return row.item.green_at ?? null;
  // A Claimed projection can exist before a thread gets a Rosen change ID; manual
  // completion still applies until an active linked change drives the Work state (#639).
  if (row.kind === 'task' && row.task.state === 'done' && !row.item?.change)
    return row.task.completed_at ?? null;
  return undefined;
}

function localDay(stamp: string | number | null) {
  const seconds =
    typeof stamp === 'string' && /^\d+$/.test(stamp) ? Number(stamp) : stamp;
  const date =
    seconds === null
      ? null
      : new Date(typeof seconds === 'number' ? seconds * 1000 : seconds);
  if (!date || !Number.isFinite(date.getTime()))
    return { key: '', label: 'Date unavailable', time: 0 };
  const day = new Date(date.getFullYear(), date.getMonth(), date.getDate());
  return {
    key: String(day.getTime()),
    label: day.toLocaleDateString(undefined, { dateStyle: 'medium' }),
    time: date.getTime(),
  };
}

function issueRowKey(
  base: string,
  item: WorkItem,
  isCurrent: boolean,
  used: Set<string>,
) {
  const suffix =
    item.change ??
    item.green_at ??
    item.landed_at ??
    item.claimed_at ??
    (item.thread_identifier != null
      ? `thread-${item.thread_identifier}`
      : 'history');
  let key = isCurrent ? base : `${base}@${suffix}`;
  if (used.has(key)) {
    let index = 2;
    while (used.has(`${key}#${index}`)) index += 1;
    key = `${key}#${index}`;
  }
  used.add(key);
  return key;
}

function buildIssueRows(
  items: WorkItem[],
  panel: IssuePanelSnapshot | null,
): WorkIssue[] {
  const panelIssues = new Map<
    string,
    { repository: string; row: IssuePanelRow }
  >();
  if (panel?.repository) {
    for (const row of panel.rows) {
      panelIssues.set(issueKey(panel.repository, row.number), {
        repository: panel.repository,
        row,
      });
    }
  }
  const issueItems = new Map<string, WorkItem[]>();
  for (const item of items) {
    if (item.key.kind !== 'issue') continue;
    const key = issueKey(item.key.repository, item.key.number);
    const list = issueItems.get(key) ?? [];
    list.push(item);
    issueItems.set(key, list);
  }
  const issueKeys = new Set([...panelIssues.keys(), ...issueItems.keys()]);
  const usedKeys = new Set<string>();
  const rows: WorkIssue[] = [];
  for (const baseKey of issueKeys) {
    const live = panelIssues.get(baseKey);
    const tracked = issueItems.get(baseKey);
    if (!tracked?.length) {
      if (live) {
        usedKeys.add(baseKey);
        rows.push({
          kind: 'issue',
          key: baseKey,
          repository: live.repository,
          number: live.row.number,
          title: live.row.title,
          row: live.row,
        });
      }
      continue;
    }
    // Pair the live issue panel row with the active/latest WorkItem while keeping earlier
    // completed items as separate historical rows so prior Green changes stay in Done (#640).
    let currentIndex = -1;
    for (let index = tracked.length - 1; index >= 0; index -= 1) {
      if (tracked[index]?.status !== 'green') {
        currentIndex = index;
        break;
      }
    }
    if (currentIndex === -1) currentIndex = tracked.length - 1;
    for (let index = 0; index < tracked.length; index += 1) {
      const item = tracked[index];
      if (!item || item.key.kind !== 'issue') continue;
      const isCurrent = index === currentIndex;
      rows.push({
        kind: 'issue',
        key: issueRowKey(baseKey, item, isCurrent, usedKeys),
        repository: item.key.repository,
        number: item.key.number,
        title: isCurrent ? (live?.row.title ?? item.title) : item.title,
        row: isCurrent ? live?.row : undefined,
        item,
      });
    }
  }
  return rows;
}

export function workGroups(
  tasks: LayoutSnapshot['tasks'],
  items: WorkItem[],
  panel: IssuePanelSnapshot | null,
) {
  const taskItems = new Map<string, WorkItem>();
  for (const item of items) {
    if (item.key.kind === 'task')
      taskItems.set(String(item.key.identifier), item);
  }
  const rows: WorkRow[] = [
    ...tasks.map((task): WorkTask => ({
      kind: 'task',
      key: `task:${task.identifier}`,
      task,
      item: taskItems.get(task.identifier),
    })),
    ...buildIssueRows(items, panel),
  ];
  const questions: WorkIssue[] = [];
  const holds: WorkIssue[] = [];
  const active: WorkRow[] = [];
  const done = new Map<
    string,
    { key: string; label: string; rows: { row: WorkRow; time: number }[] }
  >();
  for (const row of rows) {
    const stamp = completion(row);
    if (stamp !== undefined) {
      const { key, label, time } = localDay(stamp);
      const group = done.get(key) ?? { key, label, rows: [] };
      group.rows.push({ row, time });
      done.set(key, group);
    } else if (row.kind === 'issue' && row.row?.question) questions.push(row);
    else if (row.kind === 'issue' && row.row?.person_hold) holds.push(row);
    else active.push(row);
  }
  questions.sort(
    (left, right) =>
      (right.row?.question?.blocked_items ?? 0) -
        (left.row?.question?.blocked_items ?? 0) || right.number - left.number,
  );
  holds.sort((left, right) => left.number - right.number);
  return {
    questions,
    holds,
    active,
    done: [...done.values()]
      .sort((left, right) =>
        left.key && right.key
          ? Number(right.key) - Number(left.key)
          : left.key
            ? -1
            : 1,
      )
      .map((group) => ({
        ...group,
        rows: group.rows
          .sort((left, right) => right.time - left.time)
          .map(({ row }) => row),
      })),
  };
}

export function disclosureKey(
  event: KeyboardEvent,
  expanded: boolean,
  toggle: () => void,
) {
  if (
    (event.key === 'ArrowRight' && !expanded) ||
    (event.key === 'ArrowLeft' && expanded)
  ) {
    event.preventDefault();
    toggle();
  }
}
