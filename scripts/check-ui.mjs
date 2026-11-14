// Runs the UI's install, build, type check and lint, unless they already passed on exactly
// these inputs in this folder.
//
// Velocity's checker runs the pre-land commands for every approved Rosen change in one
// folder that keeps its files between checks (rosen.toml, SPEC.md F7). Most changes touch
// only Rust, and the four UI commands read nothing outside `ui/`, so running them again on
// the same `ui/` only repeats the last result, about eight seconds per change.
//
// The rule is exact, not a guess from the changed paths: the inputs are every file under
// `ui/` that is not a build product, this script, and the Node version. Their hash is
// written to a stamp only after all four commands pass, and the stamp is removed before
// any of them runs, so a stamp always describes the last full pass and the `dist` it left.
// The stamp lives in `ui/node_modules`, which no change can put a file in.
//
// usage: node scripts/check-ui.mjs [--force]

import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ui = join(root, 'ui');
const stamp = join(ui, 'node_modules', '.velocity-ui-checks');
// Build products and installed packages: outputs, not inputs.
const outputs = new Set(['node_modules', 'dist']);

function inputFiles(folder) {
  const files = [];
  for (const entry of readdirSync(folder, { withFileTypes: true })) {
    const path = join(folder, entry.name);
    if (entry.isDirectory()) {
      if (folder !== ui || !outputs.has(entry.name)) files.push(...inputFiles(path));
    } else {
      files.push(path);
    }
  }
  return files;
}

function inputsHash() {
  const hash = createHash('sha256');
  hash.update(process.version);
  const files = [fileURLToPath(import.meta.url), ...inputFiles(ui)].sort();
  for (const file of files) {
    // The path and the length frame each file, so moving bytes between files changes it.
    const bytes = readFileSync(file);
    hash.update(`\0${relative(root, file).replaceAll('\\', '/')}\0${bytes.length}\0`);
    hash.update(bytes);
  }
  return hash.digest('hex');
}

function run(command) {
  console.log(`> ${command}`);
  const result = spawnSync(command, { cwd: root, shell: true, stdio: 'inherit' });
  if (result.status !== 0) process.exit(result.status ?? 1);
}

const wanted = inputsHash();
const passed = existsSync(stamp) ? readFileSync(stamp, 'utf8').trim() : '';
if (passed === wanted && !process.argv.includes('--force')) {
  console.log(`UI checks already passed on these inputs (${wanted.slice(0, 12)}); skipped.`);
  process.exit(0);
}
rmSync(stamp, { force: true });
run('pnpm -C ui install --frozen-lockfile');
run('pnpm -C ui build');
run('pnpm -C ui check');
run('pnpm -C ui lint');
mkdirSync(dirname(stamp), { recursive: true });
writeFileSync(stamp, `${wanted}\n`);
console.log(`UI checks passed (${wanted.slice(0, 12)}).`);
