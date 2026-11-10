<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import {
    agentsCommands,
    type OutlineAction,
    type OutlineSnapshot,
  } from './ipc';
  import OutlineLine, { placeCaret } from './OutlineLine.svelte';
  import './theme/agents.css';

  let { project, saveRequests }: { project: number; saveRequests: number } =
    $props();
  let documentState = $state<OutlineSnapshot | null>(null);
  let markdown = $state(false);
  let error = $state('');
  let notice = $state('');
  let saving = $state(false);
  let queue = Promise.resolve();
  let active = true;
  let lastSave: number | undefined;
  let noticeTimer: ReturnType<typeof globalThis.setTimeout> | undefined;

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  async function receive(snapshot: OutlineSnapshot, caret?: number) {
    if (!active) return;
    documentState = snapshot;
    if (!snapshot.focus) return;
    await tick();
    const node = document.getElementById(
      `agents-line-${snapshot.focus.identifier}`,
    );
    if (!node) return;
    node.focus({ preventScroll: true });
    placeCaret(
      node,
      caret ?? (snapshot.focus.at_end ? Number.MAX_SAFE_INTEGER : 0),
    );
    node.scrollIntoView({ block: 'nearest' });
  }

  function reveal(identifier: number) {
    markdown = false;
    void tick().then(() => {
      const node = document.getElementById(`agents-line-${identifier}`);
      if (!node) return;
      node.focus({ preventScroll: true });
      placeCaret(node, Number.MAX_SAFE_INTEGER);
      node.scrollIntoView({ block: 'center' });
    });
  }

  function showNotice(message: string) {
    notice = message;
    globalThis.clearTimeout(noticeTimer);
    noticeTimer = globalThis.setTimeout(() => {
      notice = '';
    }, 2200);
  }

  function send(action: OutlineAction, caret?: number) {
    notice = '';
    queue = queue
      .then(async () => {
        error = '';
        await receive(await agentsCommands.edit(project, action), caret);
      })
      .catch(report);
  }

  let rewriting = $state<number[]>([]);
  const flaggedRules = $derived(documentState?.flagged ?? []);
  const flaggedIdentifiers = $derived(
    new Set(flaggedRules.map((rule) => rule.identifier)),
  );
  const covered = $derived(
    documentState?.coverage.filter((item) => item.covered).length ?? 0,
  );
  const rewrites = new SvelteSet<Promise<void>>();

  function submit(identifier: number) {
    queue = queue
      .then(() => {
        if (rewriting.includes(identifier)) return;
        rewriting = [...rewriting, identifier];
        const request = agentsCommands
          .tighten(project, identifier)
          .then(() => {
            queue = queue
              .then(async () => {
                await receive(await agentsCommands.read(project));
              })
              .catch(report);
          })
          .catch(report)
          .finally(() => {
            rewriting = rewriting.filter((item) => item !== identifier);
            rewrites.delete(request);
          });
        rewrites.add(request);
      })
      .catch(report);
  }

  function tightenAll() {
    if (!flaggedRules.length) {
      showNotice('Nothing flagged · every bullet is a direct rule');
      return;
    }
    for (const rule of flaggedRules) submit(rule.identifier);
  }

  function addCoverage(item: number) {
    markdown = false;
    send({ AddCoverage: { item } });
  }

  function save() {
    if (saving) return;
    saving = true;
    queue = queue
      .then(async () => {
        error = '';
        await Promise.all([...rewrites]);
        await receive(await agentsCommands.save(project));
        if (active)
          showNotice(
            'AGENTS.md saved and committed · agents pick it up on their next task',
          );
      })
      .catch(report)
      .finally(() => {
        saving = false;
      });
  }

  $effect(() => {
    if (lastSave === undefined) {
      lastSave = saveRequests;
      return;
    }
    if (saveRequests !== lastSave) {
      lastSave = saveRequests;
      save();
    }
  });

  onMount(() => {
    queue = agentsCommands.read(project).then(receive).catch(report);
    return () => {
      active = false;
      globalThis.clearTimeout(noticeTimer);
    };
  });
</script>

<section class="thread-view agents-view" aria-label="AGENTS.md editor">
  <header class="thread-head">
    <h1>AGENTS.md</h1>
    <div class="meta">
      <span>read before every task</span>{#if documentState}<span
          >{documentState.rule_count} rules</span
        ><span
          class="agents-count"
          class:warn={flaggedRules.length > 0}
          class:ok={flaggedRules.length === 0}
          >{flaggedRules.length} flagged</span
        >{#if rewriting.length}<span class="agents-busy"
            >Haiku rewriting {rewriting.length}</span
          >{/if}{#if documentState.dirty}<span>unsaved edits</span>{/if}{/if}
    </div>
    <span class="spacer"></span>
    <div class="head-actions">
      <button
        class="btn"
        class:on={markdown}
        onclick={() => {
          markdown = !markdown;
        }}
        aria-pressed={markdown}>Markdown</button
      >
      <button
        class="btn"
        title="Send every flagged bullet to the agent"
        disabled={!documentState}
        onclick={tightenAll}>Tighten all</button
      >
      <button
        class="btn primary"
        disabled={!documentState || saving}
        onclick={save}
        >{saving ? 'Saving…' : 'Save'} <kbd>Ctrl</kbd><kbd>S</kbd></button
      >
    </div>
  </header>
  {#if error}<p class="agents-notice" role="alert">{error}</p>{/if}
  {#if notice}<p class="agents-notice" role="status">{notice}</p>{/if}
  <div class="agents-body">
    <div class="agents-scroll">
      {#if !documentState}<p class="empty">Reading AGENTS.md…</p>
      {:else if markdown}<pre class="agents-raw">{documentState.markdown}</pre>
      {:else}<div class="agents-outline">
          {#each documentState.sections as section (section.identifier)}
            {#each section.lines as line (line.identifier)}
              <OutlineLine
                {line}
                {send}
                {submit}
                rewrite={documentState.rewrites.find(
                  (item) => item.identifier === line.identifier,
                )}
                pending={rewriting.includes(line.identifier)}
                flagged={flaggedIdentifiers.has(line.identifier)}
              />
            {/each}
            <button
              class="agents-add"
              onclick={() => send({ AddRule: { section: section.identifier } })}
              >+ rule</button
            >
          {/each}
          <button class="agents-add section" onclick={() => send('AddSection')}
            >+ section</button
          >
        </div>{/if}
    </div>
    <aside class="agents-help" aria-label="AGENTS.md lint">
      <p>
        Agents follow this file literally. Write one rule per bullet and press <b
          >Enter</b
        >. <b>Haiku</b> rewrites it as a direct, explicit rule and the edit
        lands straight away. Short notes get expanded. <b>Undo</b> brings back your
        wording.
      </p>
      {#if documentState}
        <div>
          <div class="agents-lint-head">
            Covers<span>{covered}/{documentState.coverage.length}</span>
          </div>
          {#each documentState.coverage as item (item.item)}
            <div class="agents-cover" class:miss={!item.covered}>
              <b>{item.covered ? '✓' : '✗'}</b><span>{item.title}</span>
              {#if !item.covered}<button
                  class="mini"
                  onclick={() => addCoverage(item.item)}>Add</button
                >{/if}
            </div>
          {/each}
        </div>
        <div>
          <div class="agents-lint-head">
            Flagged<span>{flaggedRules.length}</span>
            {#if flaggedRules.length}<button class="mini" onclick={tightenAll}
                >Tighten all</button
              >{/if}
          </div>
          {#each flaggedRules as rule (rule.identifier)}
            <button
              class="agents-flag {rule.kind.toLowerCase()}"
              onclick={() => reveal(rule.identifier)}
            >
              <span class="agents-flag-top"
                ><span class="agents-flag-section">{rule.section}</span><span
                  class="agents-flag-phrase">“{rule.phrase}”</span
                ></span
              >
              <span class="agents-flag-message">{rule.message}</span>
            </button>
          {:else}
            <p class="empty">Every bullet is a direct rule.</p>
          {/each}
        </div>
      {/if}
      <div>
        <div class="agents-lint-head">Keys</div>
        <div class="agents-keys">
          <kbd>Enter</kbd><span>submit · agent rewrites it</span>
          <span><kbd>Shift</kbd><kbd>Enter</kbd></span><span
            >next bullet, keep as typed</span
          >
          <kbd>Tab</kbd><span>make it a sub-rule</span>
          <span><kbd>Shift</kbd><kbd>Tab</kbd></span><span>outdent</span>
          <span><kbd>Alt</kbd><kbd>↑</kbd><kbd>↓</kbd></span><span
            >move the bullet</span
          >
          <kbd>## </kbd><span>start a new section</span>
          <span><kbd>Ctrl</kbd><kbd>S</kbd></span><span>save and commit</span>
        </div>
      </div>
    </aside>
  </div>
</section>
