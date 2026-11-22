<script lang="ts">
  import PaneState from './PaneState.svelte';
  import SettingsNav from './SettingsNav.svelte';
  import {
    skillSettingsCommands,
    skillLibraryCommands,
    skillPackCommands,
    onSkillPackInstallations,
    type SkillsSettingsView,
    type SkillSettingsEdit,
    type SkillPackInstallationsChanged,
    type SkillPackUpdatePreview,
    type LayoutAction,
  } from './ipc';
  import './theme/workspaces.css';
  import './theme/skills.css';
  let {
    send,
    projectIdentifier,
  }: {
    send: (action: LayoutAction) => void;
    projectIdentifier: number | null;
  } = $props();
  let view = $state<SkillsSettingsView | null>(null);
  let error = $state('');
  let busy = $state(false);
  let repository = $state('');
  let installations = $state<SkillPackInstallationsChanged>({
    revision: 0,
    packs: [],
  });
  let customInstallations = $derived(
    installations.packs.filter(
      (pack) => !pack.built_in && pack.status.state !== 'complete',
    ),
  );
  let preview = $state<SkillPackUpdatePreview | null>(null);
  let generation = 0;
  $effect(() => {
    const identifier = projectIdentifier;
    view = null;
    error = '';
    preview = null;
    void refresh(identifier);
  });
  $effect(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    function apply(next: SkillPackInstallationsChanged) {
      if (disposed || next.revision <= installations.revision) return;
      const completed = next.packs.some(
        (pack) =>
          pack.status.state === 'complete' &&
          installations.packs.find(
            (previous) => previous.pack_id === pack.pack_id,
          )?.status.state !== 'complete',
      );
      installations = next;
      if (completed) void refresh();
    }
    void onSkillPackInstallations(({ payload }) => apply(payload))
      .then(async (unlisten) => {
        if (disposed) return unlisten();
        stop = unlisten;
        apply(await skillLibraryCommands.installations());
      })
      .catch((failure: unknown) => {
        if (!disposed) error = String(failure);
      });
    return () => {
      disposed = true;
      stop?.();
    };
  });
  async function run(operation: () => Promise<void>) {
    if (busy) return;
    busy = true;
    error = '';
    try {
      await operation();
    } catch (failure) {
      error = String(failure);
    } finally {
      busy = false;
    }
  }
  async function refresh(identifier: number | null = projectIdentifier) {
    const request = ++generation;
    try {
      const result = await skillSettingsCommands.read(identifier);
      if (projectIdentifier === identifier && request === generation)
        view = result;
    } catch (failure) {
      if (projectIdentifier === identifier && request === generation)
        error = String(failure);
    }
  }
  function edit(change: SkillSettingsEdit) {
    const identifier = projectIdentifier;
    if (identifier === null) return;
    void run(async () => {
      await skillSettingsCommands.edit(identifier, change);
      if (projectIdentifier === identifier) await refresh(identifier);
    });
  }
  function install(source: string) {
    error = '';
    void skillLibraryCommands
      .installPack(source, null, null)
      .catch((failure: unknown) => {
        error = String(failure);
      });
  }
  function remove(pack: string) {
    void run(async () => {
      await skillLibraryCommands.removePack(pack);
      if (preview?.pack_id === pack) preview = null;
      await refresh();
    });
  }
  function setDefaultBundle(pack: string, bundle: string, enabled: boolean) {
    void run(async () => {
      await skillLibraryCommands.setBundleDefault(pack, bundle, enabled);
      await refresh();
    });
  }
  function generate(regenerate: boolean) {
    const identifier = projectIdentifier;
    if (identifier === null) return;
    void run(async () => {
      await skillPackCommands.generateDesignSystem(identifier, regenerate);
      await refresh(identifier);
    });
  }
  function check(identifier: string) {
    void run(async () => {
      const result = await skillLibraryCommands.checkUpdate(identifier);
      preview = result;
    });
  }
  function confirm() {
    const update = preview;
    if (!update) return;
    void run(async () => {
      await skillLibraryCommands.confirmUpdate(
        update.pack_id,
        update.candidate_commit,
      );
      preview = null;
      await refresh();
    });
  }
</script>

<section class="workspace-settings" aria-label="Skill settings">
  <header class="workspace-settings-head">
    <h1>Settings</h1>
    <span>machine-wide</span>
  </header>
  <div class="workspace-settings-body">
    <SettingsNav current="skills" {send} />
    <div class="workspace-settings-main">
      <PaneState
        name="Skills"
        network={false}
        {error}
        loading={!view}
        retry={() => run(() => refresh())}
      />
      {#if view}
        <section>
          <h2>Skill library</h2>
          <p class="workspace-description">
            Type <b>$</b> in the composer to invoke an installed skill in any project.
          </p>
          <div class="sk-budget" class:warn={view.default_budget_warning}>
            <meter
              min="0"
              max="200"
              value={view.default_skill_count}
              aria-label="Default bundle context budget"
            ></meter>
            Default bundles · {view.default_skill_count} skills · ≈{view.default_description_tokens}
            description tokens
          </div>
          {#if projectIdentifier !== null}
            <div class="sk-budget" class:warn={view.budget_warning}>
              Always on for {view.project_name} · {view.enabled_count} skills · ≈{view.description_tokens}
              description tokens per session
            </div>
          {/if}
          <div class="skill-cards">
            {#each view.cards as card (card.catalog.id)}
              {@const status = installations.packs.find(
                (pack) => pack.pack_id === card.catalog.id,
              )?.status}
              <div class="sk" class:on={card.installed}>
                <div class="top">
                  <b>{card.catalog.name}</b>
                  {#if card.suggestion_badge}<span
                      class="tag sugg"
                      data-tone="accent">{card.suggestion_badge}</span
                    >{/if}<span class="kind">{card.catalog.kind_summary}</span>
                  <span class="repo"
                    >{card.catalog.repository} · {card.catalog.license}</span
                  >
                  {#if projectIdentifier !== null && (card.installed || card.selection.enabled)}
                    <button
                      class="mini skill-toggle"
                      class:on={card.selection.enabled}
                      role="switch"
                      aria-checked={card.selection.enabled}
                      aria-label={card.installed
                        ? `Always load ${card.catalog.name} in ${view.project_name}`
                        : `Accept suggestion for ${card.catalog.name} after installation`}
                      disabled={busy}
                      onclick={() =>
                        edit({
                          Pack: {
                            identifier: card.catalog.id,
                            enabled: !card.selection.enabled,
                          },
                        })}
                      >{card.selection.enabled
                        ? card.installed
                          ? 'Always on'
                          : 'Install for always on'
                        : 'On demand'}</button
                    >
                  {/if}
                </div>
                <div class="d">{card.catalog.description}</div>
                {#if card.installed && card.catalog.bundles.length > 1}
                  <div class="meta">Default bundles</div>
                  <div class="bund">
                    {#each card.catalog.bundles as bundle (bundle.id)}
                      <button
                        class="skill-chip"
                        class:on={card.default_bundles.includes(bundle.id)}
                        aria-pressed={card.default_bundles.includes(bundle.id)}
                        disabled={busy ||
                          (card.catalog.id === 'gamedev' &&
                            bundle.id === 'router')}
                        onclick={() =>
                          setDefaultBundle(
                            card.catalog.id,
                            bundle.id,
                            !card.default_bundles.includes(bundle.id),
                          )}
                        >{bundle.label}<small>{bundle.skill_count}</small
                        ></button
                      >
                    {/each}
                  </div>
                  {#if projectIdentifier !== null && card.selection.enabled}
                    <div class="meta">
                      Always-on bundles for {view.project_name}
                    </div>
                    <div class="bund">
                      {#each card.catalog.bundles as bundle (bundle.id)}
                        <button
                          class="skill-chip"
                          class:on={card.selection.bundles.includes(bundle.id)}
                          aria-pressed={card.selection.bundles.includes(
                            bundle.id,
                          )}
                          disabled={busy ||
                            !card.installed ||
                            (card.catalog.id === 'gamedev' &&
                              bundle.id === 'router')}
                          onclick={() =>
                            edit({
                              Bundle: {
                                identifier: card.catalog.id,
                                bundle: bundle.id,
                                enabled: !card.selection.bundles.includes(
                                  bundle.id,
                                ),
                              },
                            })}
                          >{bundle.label}<small>{bundle.skill_count}</small
                          ></button
                        >
                      {/each}
                    </div>
                  {/if}
                {/if}
                <div class="meta">
                  <span
                    >{card.installed
                      ? `pinned ${card.pin}`
                      : 'Not installed'}</span
                  >
                  {#each card.requirements as requirement (requirement)}<span
                      >{requirement}</span
                    >{/each}
                  {#if card.installed}<span
                      >{card.installed_count} installed skills</span
                    >{/if}
                  {#if projectIdentifier !== null && card.detection}<span
                      >{card.detection}</span
                    >{/if}
                </div>
                {#if card.details.design_system}
                  {@const design = card.details.design_system}
                  <div class="row">
                    Design system <code>{design.design_path}</code>
                    <span>{design.summary}</span>
                    <button
                      class="mini"
                      disabled={busy}
                      onclick={() => generate(design.exists)}
                      >{design.exists ? 'Regenerate' : 'Generate'}</button
                    >
                  </div>
                {/if}
                {#if card.details.shared_server}
                  <div class="row">
                    Shared MCP server <code
                      >{card.details.shared_server.name}</code
                    >
                    <span
                      >available in this project's new sessions · IDA, Hopper
                      and JEB need your own licences</span
                    >
                  </div>
                {/if}
                <div class="row">
                  {#if card.installed}<button
                      class="mini"
                      disabled={busy}
                      onclick={() => check(card.catalog.id)}
                      >Check for updates</button
                    >
                    <button
                      class="mini ghost"
                      disabled={busy || status?.state === 'running'}
                      onclick={() => remove(card.catalog.id)}
                      >Remove pack</button
                    >
                  {:else}<button
                      class="mini primary"
                      disabled={busy || status?.state === 'running'}
                      onclick={() => install(card.catalog.id)}
                      >{status?.state === 'failed'
                        ? 'Retry install'
                        : 'Install pack'}</button
                    >{/if}
                  {#if !card.installed && status && status.state !== 'complete'}
                    <span role="status">{status.message}</span>
                  {:else}
                    <span
                      >updates are reviewed as a diff before the pin moves</span
                    >
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        </section>
        <section>
          <h2>Add a skill pack</h2>
          <p class="workspace-description">
            Any GitHub repo that follows the Agent Skills standard (a folder per
            skill with a SKILL.md).
          </p>
          <form
            class="sk-add"
            onsubmit={(event) => {
              event.preventDefault();
              install(repository);
            }}
          >
            <label class="workspace-input"
              >github.com/<input
                aria-label="Skill pack repository"
                placeholder="owner/repo"
                bind:value={repository}
              /></label
            >
            <button
              class="btn"
              disabled={!repository.trim() ||
                customInstallations.some(
                  (pack) => pack.status.state === 'running',
                )}>Add</button
            >
          </form>
          {#each customInstallations as pack (pack.pack_id)}
            {#if pack.status.state !== 'complete'}
              <p class="workspace-description" role="status">
                {pack.name} · {pack.status.message}
              </p>
            {/if}
          {/each}
        </section>
        {#if preview}<section>
            <h2>Pack update</h2>
            {#if preview.up_to_date}<p>Already up to date.</p>{:else}
              <p>{preview.current_commit} → {preview.candidate_commit}</p>
              <pre>{preview.unified_diff}</pre>
              <button class="btn primary" disabled={busy} onclick={confirm}
                >Update to reviewed commit</button
              >
            {/if}
            <button class="btn ghost" onclick={() => (preview = null)}
              >Close</button
            >
          </section>{/if}
      {/if}
    </div>
  </div>
</section>
