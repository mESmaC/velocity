import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const f8 = readFileSync('spec/F8-task-list.md', 'utf8');
const f9 = readFileSync('spec/F9-issues-and-dispatch.md', 'utf8');
const greenRule = f9.split(/\r?\n/).find(
  line => line.startsWith('Dispatch starts a linked issue') && line.includes('green integration build'),
);
const closureOnlyRule = f8.split(/\r?\n/).find(
  line => /skips GitHub issues with an open GitHub blocker and makes them eligible on the first issue refresh after their last blocker closes \(#472\)\.$/.test(line),
);

assert.ok(greenRule, 'F9 must preserve early dispatch after green integration');
assert.equal(
  closureOnlyRule,
  undefined,
  'F8 still requires closure for the open blocker that F9 already considers satisfied:\n' + greenRule,
);
