const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');

const source = readFileSync(resolve(__dirname, '../crates/velocity-core/src/tool_policy.rs'), 'utf8');
const policy = source.split('fn airc_access(')[1]?.split('fn graph_access(')[0];
const scout = policy?.split('ThreadRole::Scout => match tool {')[1]?.split('ThreadRole::FollowUp')[0];
assert.ok(scout?.includes('"issue_read"'), 'Scout policy or read control changed');

const allowsDirectFiling = scout
  .split('=>')
  .slice(0, -1)
  .some((arm, index, arms) => arm.includes('"issue_create"') &&
    /^\s*\{?\s*ToolAccess::(?:Eager|Occasional)\b/.test(scout.split('=>')[index + 1]));
console.log('Scout AIRC policy permits issue_create:', allowsDirectFiling);
assert.equal(allowsDirectFiling, false, 'F7 requires scout findings to go through scout_file');
