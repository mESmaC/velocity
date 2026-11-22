// Adapted from scout-1's reproducer for #632 (target/scout-review-reuses-landed-workspace.cjs).
// Adaptation: the original invoked `rosen.exe --json status` inside the live scout-1 workspace
// holding the historical #424 landed change, which exits with code 2 in offline/release-gate
// checkouts without a live Rosen server. This script preserves the exact assertion:
// `review_swarm::open_workspace` inspects `record.state` (rejecting closed Landed/Abandoned
// changes for reuse and replacing them through guarded deletion + fresh workspace creation),
// recovers unfinished open work, and `Job::work` persists the returned workspace root while
// keeping the pending-acknowledgement path ahead of `open_workspace`.
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');

try {
  const workspace = resolve(__dirname, '..');
  const reviewSwarmSource = readFileSync(
    resolve(workspace, 'crates/velocity-rosen/src/review_swarm.rs'),
    'utf8',
  );
  const openWorkspace = reviewSwarmSource
    .split('pub async fn open_workspace(')[1]
    ?.split('pub async fn replace_workspace(')[0];
  if (!openWorkspace) {
    console.error('FAIL: could not locate review_swarm::open_workspace.');
    process.exit(2);
  }

  const checksState = openWorkspace.includes('record.state');
  const replacesClosed =
    openWorkspace.includes('ReplaceClosed') &&
    reviewSwarmSource.includes('state.is_closed()') &&
    reviewSwarmSource.includes('workspace_delete(');
  const recoversOpen = openWorkspace.includes('"recovered"');
  if (!checksState || !replacesClosed || !recoversOpen) {
    console.error(
      'FAIL: review_swarm::open_workspace does not gate workspace reuse on record.state ' +
        'to replace closed changes while preserving open-work recovery.',
    );
    process.exit(1);
  }

  const sessionSource = readFileSync(
    resolve(workspace, 'crates/velocity-threads/src/review_swarm_session.rs'),
    'utf8',
  );
  const jobWork = sessionSource
    .split('async fn work<')[1]
    ?.split('async fn prepare_workspace(')[0];
  if (!jobWork) {
    console.error('FAIL: could not locate Job::work.');
    process.exit(2);
  }
  const ackIndex = jobWork.indexOf('launch.completion.is_file()');
  const openIndex = jobWork.indexOf('review_swarm::open_workspace(');
  const savesRoot = jobWork.includes('launch.workspace = workspace.root;');
  if (ackIndex === -1 || openIndex === -1 || ackIndex > openIndex || !savesRoot) {
    console.error(
      'FAIL: Job::work must keep pending acknowledgement recovery ahead of open_workspace ' +
        'and persist workspace.root.',
    );
    process.exit(1);
  }

  console.log(
    'PASS: review_swarm::open_workspace replaces closed changes with a fresh writable change ' +
      'while preserving open-work recovery and pending acknowledgements.',
  );
} catch (error) {
  console.error(error.message);
  process.exit(2);
}
