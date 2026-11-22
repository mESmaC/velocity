const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');

const root = resolve(__dirname, '..');
const read = (path) => readFileSync(resolve(root, path), 'utf8');
const acp = read('crates/velocity-acp/src/client/mod.rs');
const drive = acp.split('async fn drive(')[1].split('async fn ')[0];
assert.ok(drive.indexOf('emit(AgentEvent::SessionStarted') >= 0);
assert.ok(drive.indexOf('emit(AgentEvent::SessionStarted') < drive.indexOf('.drive_turns('));

const threads = read('src-tauri/src/threads.rs');
const save = threads.split('fn save_agent_session(')[1].split('\n}')[0];
assert.match(save, /AgentEvent::SessionStarted/);
assert.match(save, /thread\.agent_session = Some\(session\)/);

const store = read('crates/velocity-store/src/continuous_dispatch.rs');
const body = store.match(/pub fn failed_before_first_turn\([^{}]+\{([^{}]+)\}/)?.[1];
assert.ok(body, 'dispatch predicate changed, update the adapter');
// Evaluate the Rust guard with only the enum and Option syntax adapted.
const predicate = Function('thread', `return (${body.trim()
  .replace('ThreadState::Error', '"Error"')
  .replace(/(thread\.\w+)\.is_none\(\)/g, '($1 === null)')});`);
const state = {
  started_by_dispatch: true,
  state: 'Error',
  supervision: { turns: 0, released: false },
  agent_session: null,
  interview: null,
};
assert.equal(predicate(state), true, 'a transport failure before SessionStarted is returned');
assert.equal(predicate({ ...state, supervision: { turns: 1, released: false } }), false,
  'a completed turn is retained');

// The child exits after session/new replied but before accepting the opening prompt.
// SessionStarted reaches the pump, but there can be no completed turn or workspace work.
const returned = predicate({ ...state, agent_session: 'opened-then-disconnected' });
console.log(`After SessionStarted and before the first prompt: issue returned = ${returned}`);
assert.equal(returned, true, 'an issue with no first turn must be returned to dispatch');
