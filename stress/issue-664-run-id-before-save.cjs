// Adapted from scout-1's reproducer for #664 (target/scout-536-run-id-before-save.cjs).
// Adaptation: `QueueHost::enqueue` now awaits durable persistence (`persist_added`) before
// emitting `QueueEvent::Added(record)` and before returning `Ok(accepted)`, so persistence
// failures reject the tool call cleanly without leaking in-memory state. `QueueHost::cancel`
// likewise awaits durable persistence (`persist_cancelled`) before returning a positive
// cancellation reply, and both fresh and resumed sessions wire a `queued_run_store` backed by
// `save_pending`. The original assertion is preserved: an accepted run advertised as durable
// must already be persisted before the ACP/state event pump handles `Added`.
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');

const read = path => readFileSync(resolve(__dirname, '..', path), 'utf8');
const queue = read('crates/velocity-acp/src/client/queued_runs.rs');
const acp = read('crates/velocity-acp/src/client/mod.rs');
const threads = read('src-tauri/src/threads.rs');
const parking = read('src-tauri/src/parking.rs');
const persistence = read('crates/velocity-threads/src/queued_runs.rs');
const tool = read('crates/velocity-acp/src/client/queued_run_tool.rs');

const enqueue = queue.split(/(?:async )?fn enqueue\(/)[1].split(/\r?\n    }/)[0];
const cancel = queue.split('async fn cancel(')[1].split(/\r?\n    }/)[0];
const accept = queue.split('async fn accept(')[1].split(/\r?\n    }/)[0];
assert.match(accept, /self\.enqueue\(command, runs\)(?:\.await)?\.map\(Accepted::Queued\)/);
assert.match(accept, /self\.cancel\(query\)\.await\.map\(Accepted::Json\)/);
assert.match(accept, /socket\.write_all\(&line\)/);
assert.match(tool, /submit::<QueuedRunAccepted>/);
assert.match(queue, /Some\(QueueEvent::Added\(record\)\) => \{\s*shared\.emit\(AgentEvent::QueuedRunAdded\(record\)\)/);
assert.match(acp, /event = runs\.receive\(\) => \{\s*runs\.record\(event, self\.shared\)/);
assert.match(threads, /report\(save_queued_runs\(application, key, &batch\)\)/);
assert.match(threads, /thread\.queued_runs\.push\(record\.clone\(\)\)/);
assert.match(threads, /setup\.queued_run_store\s*=\s*Some\(queued_run_store\(application, key\)\)/);
assert.match(parking, /setup\.queued_run_store\s*=\s*Some\(crate::threads::queued_run_store\(application, key\)\)/);
assert.match(persistence, /velocity_store::save_pending\(/);

const persistedAdd = enqueue.search(/store\s*\.persist_added\(/);
const posted = enqueue.indexOf('self.events.send(QueueEvent::Added(record))');
const returned = enqueue.indexOf('Ok(accepted)');
assert.ok(posted >= 0 && posted < returned, 'Could not locate the add event and successful tool response');
const inlineBeforeReturn = enqueue
  .slice(0, returned)
  .replace(/runs\.spawn\(async move \{[\s\S]*?\r?\n\s*\}\);/, '');
const awaitsDurableAdd =
  persistedAdd >= 0 &&
  persistedAdd < returned &&
  /store\s*\.persist_added\([\s\S]*?\)\s*\.await/.test(inlineBeforeReturn);
const stored = awaitsDurableAdd ? ['velocity-run-accepted'] : [];
console.log('After a successful queue_run response, before the ACP/state event pump handles Added:');
console.log('Run IDs available for recovery:', JSON.stringify(stored));
assert.deepEqual(stored, ['velocity-run-accepted'], 'A run advertised as durable must survive this restart');

const persistedCancel = cancel.search(/store\s*\.persist_cancelled\(/);
const committedCancel = cancel.indexOf('.cancel(&query.run_id)');
assert.ok(
  persistedCancel >= 0 &&
    committedCancel > persistedCancel &&
    /store\s*\.persist_cancelled\([\s\S]*?\)\s*\.await/.test(cancel),
  'run_cancel must durably persist the cancellation before acknowledging cancelled: true',
);
