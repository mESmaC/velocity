// #130: a fragmented native notification must reach the meter while ACP retains every byte.
const { spawn, spawnSync } = require('node:child_process');
const assert = require('node:assert/strict');
const path = require('node:path');
const report = { method: 'account/rateLimits/updated', params: {
  rateLimits: { primary: { usedPercent: 21, windowDurationMins: 10080, resetsAt: 1794857720 } }
}};
const output = JSON.stringify({ id: 1, result: {} }) + '\n' + JSON.stringify(report) + '\n';
if (process.argv.includes('--fixture')) {
  process.stdout.write(output.slice(0, 75));
  setTimeout(() => process.stdout.write(output.slice(75)), 10);
} else if (process.argv.includes('--worker')) {
  const child = spawn(process.execPath, [__filename, '--fixture', 'app-server']);
  child.stdout.pipe(process.stdout);
  child.on('exit', code => { process.exitCode = code; });
} else {
  const bridge = path.resolve(__dirname, '../crates/velocity-acp/src/client/codex_limits.cjs');
  const result = spawnSync(process.execPath, ['--require', bridge, __filename, '--worker'], {
    encoding: 'utf8', timeout: 10000, windowsHide: true,
  });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, output);
  assert.equal(result.stderr.trim(), 'velocity:codex-limit ' + JSON.stringify(report.params));
  console.log('PASS: live native limits forwarded; RPC bytes preserved');
}
