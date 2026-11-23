// Risk (#692): with a legacy running release carrying no embedded commit metadata and a
// kept local build on the same release, the title bar must expose an explicit action to
// request the confirmation dialog naming both commits, while preserving strict descendant
// gating for automatic builds and ordinary verified restart offers.
//
// Adaptation from the #692 scout reproduction: resolved paths from the repository root
// instead of `process.argv[1]`, and updated the idle unknown-commit snapshot to pass the
// new `unverified_restart` offer that `LocalBuildState::view` now emits while checking
// both the Rust gates and production `LocalBuild.svelte` rendering.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { createRequire } = require('node:module');
const { pathToFileURL } = require('node:url');

const root = path.resolve(__dirname, '..');
const fromUi = createRequire(path.join(root, 'ui', 'package.json'));
const state = fs.readFileSync(
  path.join(root, 'crates', 'velocity-updates', 'src', 'local_build.rs'),
  'utf8',
);
const backend = fs.readFileSync(
  path.join(root, 'src-tauri', 'src', 'local_build.rs'),
  'utf8',
);
const source = fs.readFileSync(
  path.join(root, 'ui', 'src', 'lib', 'LocalBuild.svelte'),
  'utf8',
);

async function main() {
  assert.ok(
    state.includes('self.relation_of(&build.commit) == CommitRelation::Descendant'),
    'Ordinary verified restart offer must still require CommitRelation::Descendant',
  );
  assert.ok(
    state.includes('fn unverified_restart_offer(&self, build: &LocalBuild)'),
    'LocalBuildState::view must provide an unverified_restart offer for unknown ancestry',
  );
  assert.ok(
    state.includes('LocalBuildPhase::ConfirmRestart(prompt) => Some(prompt.clone())'),
    'LocalBuildState::view must still surface pending ConfirmRestart prompts',
  );
  assert.ok(
    backend.includes('let Some(running) = running else {\n        return CommitRelation::Unknown;'),
    'Backend ancestry check must still return CommitRelation::Unknown when running commit is missing',
  );

  const component = source
    .replace('<script lang="ts">', '<script lang="ts"> let { view: initialView } = $props();')
    .replace(/import\s*\{[^}]*\}\s*from '\.\/(?:motion|ipc)';/g, '')
    .replace(/import '\.\/theme\/usage\.css';/, '')
    .replace('$state<LocalBuildView | null>(null)', '$state<LocalBuildView | null>(initialView)');
  assert.ok(
    component.includes('$state<LocalBuildView | null>(initialView)'),
    'Cannot inject IPC snapshot into LocalBuild.svelte',
  );

  const compiled = fromUi('svelte/compiler').compile(component, {
    generate: 'server',
    runes: true,
  });
  const body = compiled.js.code.replace(
    /from (['"])(svelte\/[^'"]+)\1/g,
    (_, quote, name) => 'from ' + quote + pathToFileURL(fromUi.resolve(name)).href + quote,
  );
  const { default: LocalBuild } = await import(
    'data:text/javascript;base64,' + Buffer.from(body).toString('base64')
  );
  const { render } = await import(pathToFileURL(fromUi.resolve('svelte/server')).href);

  const empty = {
    restart: null,
    unverified_restart: null,
    previous: null,
    confirm: null,
    status: '',
    detail: '',
    failed: false,
    busy: false,
    waiting_issues: 0,
    waiting_label: '',
    waiting_detail: '',
    auto_restart: true,
  };
  const candidate = '4a126a1';
  const html = (view) => render(LocalBuild, { props: { view } }).body;

  const positive = html({
    ...empty,
    restart: { commit: candidate, label: 'Restart into ' + candidate, title: '' },
  });
  assert.ok(
    positive.includes('Restart into ' + candidate),
    'Forward restart control did not render',
  );

  const prompt = html({
    ...empty,
    confirm: {
      commit: candidate,
      running_commit: 'unknown (0.1.0-beta.19)',
      target_commit: candidate,
      message: 'Confirm restart',
    },
  });
  assert.ok(
    prompt.includes('Restart anyway') && prompt.includes(candidate),
    'Confirmation control did not render',
  );

  const unknown = html({
    ...empty,
    unverified_restart: {
      commit: candidate,
      label: 'Confirm restart into ' + candidate,
      title: 'Review and confirm restarting Velocity into local build ' + candidate,
    },
  });
  assert.ok(
    /<button\b/.test(unknown) && unknown.includes('Confirm restart into ' + candidate),
    'Title bar must render an explicit button to request confirmation when ancestry is unknown',
  );

  const idleEmpty = html(empty);
  assert.equal(
    /<button\b/.test(idleEmpty),
    false,
    'Title bar must render no button when no local build is kept',
  );
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
