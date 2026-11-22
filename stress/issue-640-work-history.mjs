import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';

const source = readFileSync('ui/src/lib/work-panel.ts', 'utf8');
const moduleUrl = 'data:text/javascript;base64,' + Buffer.from(stripTypeScriptTypes(source)).toString('base64');
const { workGroups } = await import(moduleUrl);

const repository = 'example/widget';
const number = 17;
const key = { kind: 'issue', repository, number };
const first = {
  key,
  title: 'Finish the widget',
  status: 'green',
  change: 'first-change',
  green_at: '2026-11-18T15:00:00Z',
};
const second = {
  ...first,
  status: 'claimed',
  change: 'second-change',
  green_at: null,
  thread_identifier: 12,
};
const panel = { repository, rows: [{ number, title: first.title, question: null }] };

// A second dispatch appends a Work item to the saved projection after #602.
const items = JSON.parse(JSON.stringify([first, second]));
const groups = workGroups([], items, panel);
assert.equal(groups.active.find(({ number: issue }) => issue === number)?.item.change, 'second-change');
assert.deepEqual(
  groups.done.flatMap(({ rows }) => rows.map(({ item }) => item?.change)),
  ['first-change'],
  'dispatching a new issue version must keep the prior Green change visible in Done',
);
