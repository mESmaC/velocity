const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');

const read = (path) => readFileSync(resolve(__dirname, '..', path), 'utf8');
const repair = read('crates/velocity-threads/src/build_repairs.rs');
const policy = read('crates/velocity-core/src/tool_policy.rs').split('fn browser_attachment(')[1]
  ?.split('pub fn prompt_touches_ui')[0];
const followUp = policy?.split('ThreadRole::FollowUp =>')[1]?.split('ThreadRole::Reviewer')[0];
const none = followUp?.match(/else\s*\{\s*BrowserAttachment::(\w+)/)?.[1];
assert.ok(repair.includes('plan.thread.role = velocity_core::tool_policy::ThreadRole::FollowUp;'),
  'build repairs are no longer classified as FollowUp');
assert.ok(none, 'browser attachment policy has changed, review the non-UI branch');
console.log('Browser attachment on a non-UI build repair:', none);
assert.notEqual(none, 'Omitted', 'F20 requires velocity-browser to be available to implementation threads');
