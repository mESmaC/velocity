const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');

const read = path => readFileSync(resolve(__dirname, '..', path), 'utf8');
const body = (source, name) => {
  const match = source.match(new RegExp('(?:async )?fn ' + name + '\\([\\s\\S]*?\\r?\\n    }'));
  assert.ok(match, 'Could not locate ' + name);
  return match[0];
};

const acp = read('crates/velocity-acp/src/client/mod.rs');
const continuation = body(acp, 'continue_after_runs');
const request = body(acp, 'request');
const queue = read('crates/velocity-acp/src/client/queued_runs.rs');
const persistence = read('src-tauri/src/threads.rs');
assert.match(request, /send_request\(PromptRequest::new/);
assert.match(continuation, /self\.apply\(effect\)/);
assert.match(persistence, /AgentEvent::QueuedRunsDelivered \{ ids \} =>[\s\S]*?record\.delivered = true/);
assert.match(queue, /\.filter\(\|record\| !record\.delivered\)/);

const send = continuation.indexOf('self.apply(effect)', continuation.indexOf('self.preface.append'));
assert.ok(send >= 0, 'Could not locate the provider prompt dispatch');
const acknowledge = continuation.indexOf('.emit(AgentEvent::QueuedRunsDelivered');
const stored = [{ id: 'velocity-run-finished', delivered: false }];
const selectedAfterRestart = () => stored.filter(record => !record.delivered).map(record => record.id);
assert.deepEqual(selectedAfterRestart(), ['velocity-run-finished'], 'Control: an undelivered result is recoverable');

console.log('Interleaving: the thread store handles pending events before the provider prompt is sent.');
if (acknowledge >= 0 && acknowledge < send) {
  stored[0].delivered = true;
}
console.log('Completed runs selected for the restart notice:', JSON.stringify(selectedAfterRestart()));
assert.deepEqual(selectedAfterRestart(), ['velocity-run-finished'], 'A result whose prompt has not been sent must be included after restart');
