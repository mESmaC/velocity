const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.resolve(__dirname, '..');
const read = (name) => fs.readFileSync(path.join(root, name), 'utf8').replace(/\r\n/g, '\n');
const threads = read('src-tauri/src/threads.rs');
const session = read('crates/velocity-acp/src/client/session.rs');
const startup = threads.split('async fn run_question_session(')[1]?.split('/// F9')[0];
const registration = session.split('fn thread_initialize_request(')[1]?.split('/// AIRC')[0];
const capabilities = session.split('fn initialize_request_with_write(')[1]?.split('/// A thread session')[0];

assert.ok(startup?.includes('setup.read_only = true;'), 'question launch no longer uses the same setup');
assert.ok(registration?.includes('initialize_request_with_write(!shared.read_only)'), 'review the changed ACP setup');
assert.ok(capabilities?.includes('.write_text_file(allow_write)'), 'review the changed ACP capabilities');

const allowsTerminal = /^\s*\.terminal\(true\)$/m.test(capabilities);
console.log('Question capabilities from the ACP initialization path:', {
  writeTextFile: false, terminal: allowsTerminal,
});
assert.equal(allowsTerminal, false, 'A read-only question thread must not advertise an unrestricted terminal');
