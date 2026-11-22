const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');

const workspace = resolve(__dirname, '..');
const read = (path) => readFileSync(resolve(workspace, path), 'utf8');
const runner = read('crates/velocity-threads/src/landing_runner.rs');
const design = read('proofs/TargetedRebase/Design.lean');
const fixture = read('proofs/fixtures/targeted-rebase.tsv');
const categories = [...new Set(fixture.trim().split(/\r?\n/).map((row) => row.split('\t')[0]))];
const refusalPathIsLive =
  runner.includes('self.report_refusal(') &&
  runner.includes('retry_later(key, generation)') &&
  runner.includes('host.land_refused(');
const existingCategories = ['rebase', 'pre_land', 'post_land', 'landing_request'];

if (!refusalPathIsLive || categories.length === 0) {
  console.error('Cannot inspect the expected live Rosen refusal path and fixture export.');
  process.exitCode = 2;
} else if (categories.every((category) => existingCategories.includes(category)) &&
           !/\brefusal\w*\b/i.test(design)) {
  console.error('FAIL: the live landing runner retries or returns a refusal by named generation. TargetedRebase has no refusal model and its fixture rows are only ' + categories.join(', ') + '.');
  process.exitCode = 1;
} else {
  console.log('The original omission is absent. Inspect the new model and fixture coverage.');
}
