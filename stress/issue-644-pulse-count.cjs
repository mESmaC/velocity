// Adapted from scout-1's reproducer for #644 (target/scout-497-main-window.cjs, `pulse-count`
// case). Adaptation: isolates the `pulse-count` probe from the multi-case scout script so the
// stress runner can invoke it with no arguments, and preserves all three source assertions
// verbatim.
const { readFileSync } = require('node:fs');

function read(path) {
  return readFileSync(path, 'utf8').replace(/\r\n/g, '\n');
}

function section(source, start, end) {
  const first = source.indexOf(start);
  const last = source.indexOf(end, first + start.length);
  if (first < 0 || last < 0) throw new Error('Missing source anchors: ' + start);
  return source.slice(first, last);
}

function check(label, condition) {
  if (condition) {
    console.log('PASS: ' + label);
  } else {
    console.error('FAIL: ' + label);
    process.exitCode = 1;
  }
}

const storePath = 'crates/velocity-store/src/rosen.rs';
const record = section(read(storePath), 'fn record_rosen_progress(', 'fn already_applied(');
const detached = section(
  record,
  'let Some(index) = thread else {',
  'let identifier = item.threads[index].id;',
);
const readModel = read('crates/velocity-store/src/project_layout.rs');

check(
  'The no-thread path acknowledges the project event and saves before returning',
  record.includes('item.rosen_event_cursor = item.rosen_event_cursor.max(seq)') &&
    detached.includes('self.store_snapshot(next)?') &&
    detached.includes('return Ok(None)'),
);
check(
  'A project land without an owning Velocity thread can reach the pulse count',
  record.indexOf('.record_land(') < record.indexOf(detached) ||
    detached.includes('.record_land(') ||
    !readModel.includes('project.landed_on(crate::landed_today::local_day())'),
);
check(
  'A land replayed after midnight can use its occurrence day',
  !record.includes('item.record_land(crate::landed_today::local_day())'),
);
