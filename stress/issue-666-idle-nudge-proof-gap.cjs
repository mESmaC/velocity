const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');

const read = path => readFileSync(resolve(__dirname, '..', path), 'utf8');
const rust = read('crates/velocity-threads/src/agent_activity.rs');
const lean = read('proofs/AgentActivity/Design.lean');
const fixtures = read('proofs/fixtures/agent-activity.tsv');
const idleDue = rust.split('fn idle_working_due(')[1].split(/\r?\n}/)[0];
const gate = idleDue.match(/&& (!thread\.queued_run_history\.iter\(\)\.any\(\|run\| !run\.delivered\))/);
assert.ok(gate, 'Could not locate the pending-result gate in idle_working_due');
const rustAllows = Function('thread', 'return ' + gate[1].replace('.iter().any(|run| ', '.some(run => '));

const base = { time: 2200, status: 'live', pullRequest: false };
const delivered = { ...base, queued_run_history: [{ delivered: true }] };
const pending = { ...base, queued_run_history: [{ delivered: false }] };
assert.equal(rustAllows(delivered), true, 'Control: a delivered result does not prevent an otherwise due nudge');
assert.equal(rustAllows(pending), false, 'An undelivered result prevents the nudge');

const observation = lean.split('structure Observation where')[1].split('deriving Repr')[0];
const fields = [...observation.matchAll(/^\s*(\w+)\s*:/gm)].map(match => match[1]);
const modelInput = state => JSON.stringify(fields.map(field => state[field]));
console.log('Model observation fields:', fields.join(', '));
console.log('Fixture row kinds:', [...new Set(fixtures.trim().split(/\r?\n/).map(row => row.split('\t')[0]))].join(', '));
console.log('Idle nudge eligibility at the deadline: delivered=' + rustAllows(delivered) + ' undelivered=' + rustAllows(pending));
assert.notEqual(modelInput(delivered), modelInput(pending), 'The design and fixture need to distinguish these supervision inputs');
