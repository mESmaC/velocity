const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');

// Check the diagnostic lifetime at the source boundary. A tab switch must not
// discard diagnostics already accepted by the active CDP session.
const read = (name) => readFileSync('crates/velocity-browser/src/' + name, 'utf8');
const page = read('page.rs');
const cdp = read('cdp.rs');
const tools = read('tools.rs');
const switchTab = page.match(/pub async fn switch\b[\s\S]+?(?=    pub async fn )/)?.[0];
assert.ok(switchTab, 'Could not locate the active tab switch');

const reset = switchTab.match(/\*events\s*=\s*crate::cdp::Events\s*\{([^}]+)\}/)?.[1];
const buffers = ['console', 'failures'];
const discarded = buffers.filter((name) =>
  reset?.includes('..Default::default()') &&
  new RegExp('pub ' + name + ': VecDeque<String>').test(cdp) &&
  new RegExp('events\\.' + name + '\\.drain\\(\\.\\.\\)').test(tools) &&
  !new RegExp('\\b' + name + '\\b').test(switchTab)
);
if (discarded.length) {
  console.error('FAIL: Page::switch resets unread ' + discarded.join(' and ') +
    ' buffers when changing CDP sessions. The MCP tools drain those same buffers.');
  process.exitCode = 1;
} else {
  console.log('The original tab-switch diagnostic reset is absent.');
}
