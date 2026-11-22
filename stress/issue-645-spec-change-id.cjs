// Copied from the scout reproducer for #645 (target/scout-497-main-window.cjs).
// Adaptation: defaults process.argv[2] to 'spec-change-id' when invoked without arguments;
// all assertions from the original scout script are preserved unchanged.
// Post-land review of #497. These probes inspect the implementation's control flow
// in the supplied checkout without launching the desktop app or modifying state.
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

const layoutPath = 'crates/velocity-core/src/layout_state.rs';
const storePath = 'crates/velocity-store/src/rosen.rs';

switch (process.argv[2] ?? 'spec-change-id') {
  case 'draft-jump': {
    const layout = read(layoutPath);
    const draft = section(layout, 'fn draft_group(', 'fn sidebar_rows(');
    const jump = section(layout, 'NavigationIntent::Jump(number) => {', 'NavigationIntent::NewThread => {');
    const sidebar = read('ui/src/lib/Sidebar.svelte');
    check('The draft occupies the first visible shortcut slot and clicking it requests NewThread',
      draft.includes('identifier: layout_catalog::DRAFT_ROW') &&
      draft.includes('number: Some(*number)') &&
      /row\.group === 'Draft'\s*\?\s*\{ Request: \{ Navigation: 'NewThread' \} \}/.test(sidebar));
    check('Alt+1 on the draft slot can reach NewThread from a non-Home view',
      !/\.map\(\|row\| row\.identifier\)/.test(jump) ||
      !jump.includes('.and_then(|identifier| self.open_thread(identifier))') ||
      section(layout, 'fn open_thread(', 'fn selected_row(').includes('DRAFT_ROW'));
    break;
  }
  case 'pulse-count': {
    const record = section(read(storePath), 'fn record_rosen_progress(', 'fn already_applied(');
    const detached = section(record, 'let Some(index) = thread else {', 'let identifier = item.threads[index].id;');
    const readModel = read('crates/velocity-store/src/project_layout.rs');
    check('The no-thread path acknowledges the project event and saves before returning',
      record.includes('item.rosen_event_cursor = item.rosen_event_cursor.max(seq)') &&
      detached.includes('self.store_snapshot(next)?') && detached.includes('return Ok(None)'));
    check('A project land without an owning Velocity thread can reach the pulse count',
      record.indexOf('.record_land(') < record.indexOf(detached) ||
      detached.includes('.record_land(') ||
      !readModel.includes('project.landed_on(crate::landed_today::local_day())'));
    check('A land replayed after midnight can use its occurrence day',
      !record.includes('item.record_land(crate::landed_today::local_day())'));
    break;
  }
  case 'spec-change-id': {
    const component = read('ui/src/lib/SpecInterview.svelte');
    const header = section(component, '<header class="thread-head">', '<div class="head-actions">');
    const store = read('crates/velocity-store/src/project_layout.rs');
    const interview = read('src-tauri/src/interview.rs');
    const model = read('crates/velocity-core/src/thread_view_model.rs');
    check('Rosen spec workspace setup saves the change identifier on the thread',
      interview.includes('thread.rosen_change = Some(created.change.clone())'));
    const rendersBranch = /thread\.branch\b/.test(header) ||
      (header.includes('thread.change_label') && model.includes('change_label: entry.branch.clone()'));
    check('The spec interview meta reads the saved Rosen change identifier',
      !rendersBranch || !store.includes('branch: thread.branch_name.clone()') ||
        /thread\.rosen_change\b/.test(header));
    break;
  }
  case 'pulse-zeros': {
    const pulse = section(read(layoutPath), 'fn pulse(&self)', 'fn ');
    const titleBar = read('ui/src/lib/TitleBar.svelte');
    const spec = read('spec/F1-main-window.md');
    const reference = read('design/main-window.html');
    check('The spec and prototype both make visibility depend on a nonzero count',
      spec.includes('each shown only when it is non-zero') &&
      reference.includes('const seg = (s, label, col) => c(s) ?') &&
      reference.includes('state.landedToday ?'));
    check('The landed title bar meets the source issue discussion’s requirement to show zero-count pills',
      !(/count\s*(?:==\s*0|!=\s*0|>\s*0)/.test(pulse) &&
        titleBar.includes('layout?.pulse.length')));
    break;
  }
  default:
    throw new Error('Expected draft-jump, pulse-count or pulse-zeros');
}
