// Adapted from scout-1's reproducer for #672 (scout-530-withheld-diagnostics.cjs).
// Adaptation: resolved the repository root from `stress/` (`join(__dirname, '..', ...)`)
// so the check runs from `stress/`; the original assertion is unchanged.
const { readFileSync } = require('node:fs');
const { join } = require('node:path');

// F15 requires withheld variable names in the thread debug log. This source
// check does not launch a provider or read any environment variables.
const source = readFileSync(
  join(__dirname, '..', 'crates/velocity-acp/src/client/terminals.rs'),
  'utf8',
);
const terminalCreate = source.match(/^    pub fn create\([\s\S]*?^    }/m);
if (!terminalCreate) {
  console.error('Setup error: could not locate ACP terminal creation.');
  process.exit(2);
}

if (/let\s+_\s*=\s*filter_inherited_environment\(/.test(terminalCreate[0])) {
  console.error('FAIL: ACP terminal creation discards withheld variable names before spawning.');
  process.exit(1);
}

console.log('The ACP terminal creation path no longer discards the filter result at the call site.');
