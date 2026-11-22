// Adapted from scout-1's reproducer for #629 (target/scout-targeted-rebase-stale-scope.cjs).
// Adaptation: the original invoked `rosen.exe diff --change okpnrxokzzqsxvszppyrtwmyknsrxxoy`
// and `rg`, which require a live Rosen server holding that historical change and ripgrep on PATH
// (failing with exit 2 in offline/release-gate checkouts). This script preserves the exact
// assertion: `crates/velocity-github/src/landing.rs` is gone, `lean_landing_fixtures` no longer
// tests the retired `check_scope` helper, and every live Rosen check helper asserted by
// `lean_landing_fixtures` (`pre_land_check_plan`, `post_land_check_plan`) has a production caller
// outside `crates/velocity-core/src/landing.rs`.
const { existsSync, readdirSync, readFileSync } = require('node:fs');
const { join, relative, resolve } = require('node:path');

function walkRust(directory, results = []) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.name === 'target' || entry.name === '.git' || entry.name === '.rosen') continue;
    const full = join(directory, entry.name);
    if (entry.isDirectory()) {
      walkRust(full, results);
    } else if (entry.name.endsWith('.rs')) {
      results.push(full);
    }
  }
  return results;
}

try {
  const workspace = resolve(__dirname, '..');
  const fixturePath = 'crates/velocity-core/src/landing.rs';
  const retiredPath = resolve(workspace, 'crates/velocity-github/src/landing.rs');
  const fixture = readFileSync(resolve(workspace, fixturePath), 'utf8');

  if (!existsSync(retiredPath) && /lean_landing_fixtures[\s\S]*\bsuper::check_scope\(/.test(fixture)) {
    console.error('FAIL: #424 deleted crates/velocity-github/src/landing.rs, but lean_landing_fixtures still tests retired check_scope.');
    process.exit(1);
  }

  const rustFiles = walkRust(resolve(workspace, 'crates'));
  for (const helper of ['pre_land_check_plan', 'post_land_check_plan']) {
    const fixturePattern = new RegExp(`lean_landing_fixtures[\\s\\S]*\\bsuper::${helper}\\(`);
    if (!fixturePattern.test(fixture)) {
      console.error(`FAIL: lean_landing_fixtures does not verify live Rosen helper ${helper}.`);
      process.exit(1);
    }
    const callPattern = new RegExp(`\\b${helper}\\s*\\(`);
    const productionCallers = rustFiles
      .map(file => relative(workspace, file).replace(/\\/g, '/'))
      .filter(rel => rel !== fixturePath)
      .filter(rel => callPattern.test(readFileSync(resolve(workspace, rel), 'utf8')));
    if (productionCallers.length === 0) {
      console.error(`FAIL: ${helper} is tested by lean_landing_fixtures but has no production caller outside ${fixturePath}.`);
      process.exit(1);
    }
  }

  console.log('PASS: TargetedRebase fixtures verify live Rosen pre-land and post-land check policies.');
} catch (error) {
  console.error(error.message);
  process.exit(2);
}
