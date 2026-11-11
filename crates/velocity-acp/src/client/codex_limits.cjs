// Codex ACP currently consumes account/rateLimits/updated without forwarding it.
// Observe its app-server child without changing either side of the RPC stream.
const childProcess = require('node:child_process');
const { syncBuiltinESMExports } = require('node:module');
const originalSpawn = childProcess.spawn;
childProcess.spawn = function (...arguments_) {
  const child = originalSpawn.apply(this, arguments_);
  if (!Array.isArray(arguments_[1]) || !arguments_[1].includes('app-server') || !child.stdout) return child;
  let pending = '';
  let dropping = false;
  child.stdout.on('data', (chunk) => {
    pending += chunk.toString('utf8');
    let end;
    while ((end = pending.indexOf('\n')) >= 0) {
      const line = pending.slice(0, end);
      pending = pending.slice(end + 1);
      if (!dropping) report(line);
      dropping = false;
    }
    // Tool results can be huge; telemetry must never retain an unbounded line.
    if (pending.length > 1024 * 1024) {
      pending = '';
      dropping = true;
    }
  });
  return child;
};
syncBuiltinESMExports();

function report(line) {
  try {
    const notification = JSON.parse(line);
    if (notification.method !== 'account/rateLimits/updated') return;
    process.stderr.write('velocity:codex-limit ' + JSON.stringify(notification.params) + '\n');
  } catch {
    // Non-JSON output and unsupported reports have no effect on the session.
  }
}
