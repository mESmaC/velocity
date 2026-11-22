import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';

const source = readFileSync('ui/src/lib/work-panel.ts', 'utf8');
const moduleUrl = 'data:text/javascript;base64,' + Buffer.from(stripTypeScriptTypes(source)).toString('base64');
const { workGroups } = await import(moduleUrl);

const identifier = 7;
const completedAt = 1795273200;
const claimed = {
  key: { kind: 'task', identifier },
  title: 'Check the smoke result',
  status: 'claimed',
  thread_identifier: 42,
  change: null,
};
const task = {
  identifier: String(identifier),
  title: claimed.title,
  state: 'done',
  thread_identifier: '42',
  completed_at: completedAt,
};

// A Task List update to Done can save while a claim is waiting for a workspace.
// The persisted Work item exists, but the task has no active Rosen change.
const restored = JSON.parse(JSON.stringify({ tasks: [task], items: [claimed] }));
const groups = workGroups(restored.tasks, restored.items, null);
assert.equal(
  groups.done.flatMap(({ rows }) => rows).filter(({ key }) => key === 'task:7').length,
  1,
  'a manually completed task with no active Rosen change belongs in Done',
);
assert.equal(groups.active.some(({ key }) => key === 'task:7'), false);
