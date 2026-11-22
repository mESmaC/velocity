// Adapted from scout-1's reproducer for #659 (target/scout-514-workspace-marker.cjs).
// Adaptation: `prepare_root` now takes `(root, change)` instead of `(root)` and calls
// `verify_workspace_change(root, change)?` before returning `Ok(true)`. The marker fixture
// also falls back to a v1 workspace marker template when the checkout has no `.rosen`
// directory (as in git release-gate checkouts). The original assertion is preserved:
// a foreign workspace marker must never complete the requested launch.
const assert = require('node:assert/strict');
const { existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join, resolve } = require('node:path');

const root = resolve(__dirname, '..');
const source = readFileSync(join(root, 'crates/velocity-rosen/src/workspace.rs'), 'utf8');
const open = source.split('pub async fn open_workspace_within(')[1]?.split('let shared =')[0];
assert.ok(open, 'bounded workspace opener changed, update the adapter');
const shortcut = open.match(/if prepare_root\(root(?:,\s*change)?\)\?\s*\{([\s\S]+?)\n\s*\}/)?.[1];
assert.ok(shortcut, 'marker shortcut changed, update the adapter');
const prepare = source.split('fn prepare_root(')[1]?.split('return Ok(true);')[0];
assert.ok(prepare, 'prepare_root changed, update the adapter');
const verify = source.split('pub fn verify_workspace_change(')[1]?.split('\n}\n')[0] ?? '';
const verifiesMarkerChange =
  /verify_workspace_change\(root,\s*change\)\?/.test(prepare) &&
  verify.includes('WorkspaceMarker::read(root)') &&
  verify.includes('existing != expected');
const markerOnly =
  (/if (?:has_workspace\(root\)|rosen_client::snapshot::WorkspaceMarker::path\(root\)\.is_file\(\))\s*\{$/.test(prepare.trim()) ||
    !verifiesMarkerChange) &&
  /AgentWorkspace::new\(\s*change\.to_owned\(\),\s*root,\s*"reopened",/.test(shortcut);

const scratch = mkdtempSync(join(tmpdir(), 'velocity-514-marker-'));
try {
  const markerPath = join(scratch, '.rosen', 'workspace.json');
  mkdirSync(join(scratch, '.rosen'));
  const repoMarkerPath = join(root, '.rosen', 'workspace.json');
  const marker = existsSync(repoMarkerPath)
    ? JSON.parse(readFileSync(repoMarkerPath, 'utf8'))
    : {
        schema: 'rosen.workspace/v1',
        change: 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
        tree: '0'.repeat(40),
        sparse: [],
        ignore: [],
      };
  marker.change = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
  writeFileSync(markerPath, JSON.stringify(marker));
  const requested = 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
  // Evaluate the marker-only fast path against an existing workspace on disk.
  const accepted = markerOnly && statSync(markerPath).isFile() ? requested : null;
  console.log(`On-disk change ${marker.change}; requested ${requested}; reported ${accepted}`);
  assert.equal(accepted, null, 'a foreign workspace marker must not complete the requested launch');
} finally {
  rmSync(scratch, { recursive: true });
}
