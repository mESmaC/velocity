// Adapted from scout-1's reproducer for #665 (target/scout-536-queue-counts-self.cjs).
// Adaptation: `QueueHost::status` now consults the run's live Rosen `QueueTicket::estimate`
// (which reads `waiter.ahead()` and passes `session_pending_ahead = 0` to
// `estimate_ordinary_entries`) before falling back to `self.estimate_queue(...)`, and
// `counts_as_session_pending_ahead` skips runs that already hold an active Rosen ticket.
// The original assertion is preserved: a run whose own ticket is the only waiting entry
// reports position 1, and later same-class waiters do not inflate its position.
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');

const read = path => readFileSync(resolve(__dirname, '..', path), 'utf8');
const turn = read('crates/velocity-rosen/src/queued_turn.rs');
const acp = read('crates/velocity-acp/src/client/queued_runs.rs');
const command = read('crates/velocity-acp/src/client/queued_run_command.rs');

assert.match(turn, /let waiter = queue\.wait\(target_class, note\)\?/);
assert.match(command, /acquire_with_ticket\(/);
assert.match(command, /ticket_sink\.set_ticket\(ticket\)/);
assert.match(acp, /\.pending_count_before\(Some\(&run_id\), policy, record\.class\)/);
assert.match(acp, /self\.estimate_queue\(policy, record\.class, pending_ahead\)/);

const statusBody = acp.split('async fn status(')[1]?.split('\n    fn ')[0] ?? '';
const usesTicketEstimate =
  /ticket\.as_ref\(\)\.and_then\(QueueTicket::estimate\)/.test(statusBody) &&
  /waiter\.ahead\(\)/.test(turn.split('impl QueueTicket')[1]?.split('\n}\n')[0] ?? '');
const skipsActiveTicketInSessionCount =
  /run\.ticket\.as_ref\(\)\.is_some_and\(QueueTicket::is_active\)/.test(acp);
assert.ok(
  skipsActiveTicketInSessionCount,
  'Session pending count must skip runs that already hold an active Rosen ticket'
);

const estimate = turn.split('fn estimate_ordinary_entries(')[1]?.split(/\r?\n}/)[0] ?? '';
const count = estimate.match(
  /let waiting_ahead = entries\s*\.iter\(\)\s*\.filter\(\|entry\| ([^\n]+)\)\s*\.count\(\)\s*\.saturating_add\(session_pending_ahead\);/
);
assert.ok(count, 'Update the adapter for the production estimator');
const included = Function('entry', 'target_class', 'return (' + count[1] + ')');
assert.match(turn, /let position = u32::try_from\(waiting_ahead\.saturating_add\(1\)\)/);

const targetClass = 2;
const visibleToStatus = (entries, queriedId) => {
  if (!usesTicketEstimate) {
    return entries;
  }
  const index = entries.findIndex(entry => entry.id === queriedId);
  const ahead = index >= 0 ? entries.slice(0, index) : entries;
  return ahead.filter(entry => entry.running || entry.class <= targetClass);
};
const test = (entries, queriedId = 'the-run-being-queried') =>
  visibleToStatus(entries, queriedId).filter(entry => included(entry, targetClass)).length + 1;

assert.equal(test([]), 1, 'Control: before joining the queue the first run reports position 1');

const onlyWaiter = { id: 'the-run-being-queried', running: false, class: targetClass };
const position = test([onlyWaiter], onlyWaiter.id);
console.log('Rosen waiting line: one ticket, owned by the queried run. Reported position:', position);
assert.equal(position, 1, 'The run itself is not a run ahead of it');

const laterWaiter = { id: 'later-same-class', running: false, class: targetClass };
assert.equal(
  test([onlyWaiter, laterWaiter], onlyWaiter.id),
  1,
  'Later same-class waiters must not inflate an earlier ticket position'
);

const earlierWaiter = { id: 'earlier-same-class', running: false, class: targetClass };
assert.equal(
  test([earlierWaiter, onlyWaiter, laterWaiter], onlyWaiter.id),
  2,
  'One earlier same-class waiter ahead gives position 2'
);
