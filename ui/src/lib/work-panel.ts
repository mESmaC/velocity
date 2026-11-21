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
  if (row.kind === 'task' && row.task.state === 'done' && !row.item)
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

export function workGroups(
  tasks: LayoutSnapshot['tasks'],
  items: WorkItem[],
  panel: IssuePanelSnapshot | null,
) {
  const taskItems = new Map<string, WorkItem>();
  const issues = new Map<string, WorkIssue>();
  if (panel?.repository) {
    for (const row of panel.rows) {
      const key = issueKey(panel.repository, row.number);
      issues.set(key, {
        kind: 'issue',
        key,
        repository: panel.repository,
        number: row.number,
        title: row.title,
        row,
      });
    }
  }
  for (const item of items) {
    if (item.key.kind === 'task')
      taskItems.set(String(item.key.identifier), item);
    if (item.key.kind === 'issue') {
      const { repository, number } = item.key;
      const key = issueKey(repository, number);
      const previous = issues.get(key);
      issues.set(key, {
        kind: 'issue',
        key,
        repository,
        number,
        title: previous?.title ?? item.title,
        row: previous?.row,
        item,
      });
    }
  }
  const rows: WorkRow[] = [
    ...tasks.map((task): WorkTask => ({
      kind: 'task',
      key: `task:${task.identifier}`,
      task,
      item: taskItems.get(task.identifier),
    })),
    ...issues.values(),
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
