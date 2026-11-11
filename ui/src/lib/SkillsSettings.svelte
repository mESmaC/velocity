<script lang="ts">
  import SettingsNav from './SettingsNav.svelte';
  import {
    skillSettingsCommands,
    skillLibraryCommands,
    skillPackCommands,
    type SkillsSettingsView,
    type SkillSettingsEdit,
    type SkillPackUpdatePreview,
    type LayoutAction,
  } from './ipc';
  import './theme/worktrees.css';
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
  let preview = $state<SkillPackUpdatePreview | null>(null);
  let generation = 0;
  $effect(() => {
    const identifier = projectIdentifier;
    const request = ++generation;
    view = null;
    preview = null;
    if (identifier !== null) {
      skillSettingsCommands
        .read(identifier)
        .then((result) => {
          if (request !== generation) return;
          view = result;
        })
        .catch((failure) => {
          if (request === generation) error = String(failure);
        });
    }
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
  async function refresh(identifier: number) {
    const result = await skillSettingsCommands.read(identifier);
    if (projectIdentifier !== identifier) return;
    view = result;
  }
  function edit(change: SkillSettingsEdit) {
    const identifier = projectIdentifier;
    if (identifier === null) return;
    void run(async () => {
      const result = await skillSettingsCommands.edit(identifier, change);
      if (projectIdentifier === identifier) view = result;
    });
  }
  function install(source: string) {
    const identifier = projectIdentifier;
    if (identifier === null) return;
    void run(async () => {
      await skillLibraryCommands.installPack(source, null, null);
      await refresh(identifier);
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
    const identifier = projectIdentifier;
    if (!update || identifier === null) return;
    void run(async () => {
      await skillLibraryCommands.confirmUpdate(
        update.pack_id,
        update.candidate_commit,
      );
      preview = null;
      await refresh(identifier);
    });
  }
</script>

<section class="worktree-settings" aria-label="Skill settings">
  <header class="worktree-settings-head">
    <h1>Settings</h1>
    <span>current project</span>
  </header>
  <div class="worktree-settings-body">
    <SettingsNav current="skills" {send} />
    <div class="worktree-settings-main">
      {#if error}<p role="alert">{error}</p>{/if}
      {#if view}
        <section>
          <h2>Skills for {view.project_name}</h2>
          <p class="worktree-description">
            Skill packs are installed once and switched on per project. Every
            agent in this project uses the same selection.
          </p>
          <div class="sk-budget" class:warn={view.budget_warning}>
            <meter
              min="0"
              max="200"
              value={view.enabled_count}
              aria-label="Enabled skills"
            ></meter>
            {view.enabled_count} skills on · ≈{view.description_tokens} tokens of
            skill descriptions every agent loads
            {#if view.budget_warning}
              · turn off bundles this project doesn’t use{/if}
          </div>
          <div class="skill-cards">
            {#each view.cards as card (card.catalog.id)}
              <div
                class="sk"
                class:on={card.installed && card.selection.enabled}
              >
                <div class="top">
                  <b>{card.catalog.name}</b>
                  {#if card.suggestion_badge}<span class="sugg"
                      >{card.suggestion_badge}</span
                    >{/if}<span class="kind">{card.catalog.kind_summary}</span>
                  <span class="repo"
                    >{card.catalog.repository} · {card.catalog.license}</span
                  >
                  <button
                    class="skill-toggle"
                    role="switch"
                    aria-checked={card.selection.enabled}
                    aria-label={card.installed
                      ? `Enable ${card.catalog.name}`
                      : `Accept suggestion for ${card.catalog.name} after installation`}
                    disabled={busy ||
                      (!card.installed && !card.selection.enabled)}
                    onclick={() =>
                      edit({
                        Pack: {
                          identifier: card.catalog.id,
                          enabled: !card.selection.enabled,
                        },
                      })}
                    >{card.selection.enabled
                      ? card.installed
                        ? 'On'
                        : 'Install to turn on'
                      : 'Off'}</button
                  >
                </div>
                <div class="d">{card.catalog.description}</div>
                {#if card.catalog.bundles.length > 1}
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
                <div class="meta">
                  <span
                    >{card.installed
                      ? `pinned ${card.pin}`
                      : 'Not installed'}</span
                  >
                  {#each card.requirements as requirement (requirement)}<span
                      >{requirement}</span
                    >{/each}
                  <span>{card.enabled_count} skills on</span>
                  {#if card.detection}<span>{card.detection}</span>{/if}
                </div>
                {#if card.details.design_system}
                  {@const design = card.details.design_system}
                  <div class="row">
                    Design system <code>{design.master_path}</code>
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
                      >passed to every agent of every provider (§3.2 Tool
                      access) · IDA, Hopper and JEB need your own licences</span
                    >
                  </div>
                {/if}
                <div class="row">
                  {#if card.installed}<button
                      disabled={busy}
                      onclick={() => check(card.catalog.id)}
                      >Check for updates</button
                    >
                  {:else}<button
                      disabled={busy}
                      onclick={() => install(card.catalog.id)}
                      >Install pack</button
                    >{/if}
                  <span
                    >updates are reviewed as a diff before the pin moves</span
                  >
                </div>
              </div>
            {/each}
          </div>
        </section>
        <section>
          <h2>Add a skill pack</h2>
          <p class="worktree-description">
            Any GitHub repo that follows the Agent Skills standard (a folder per
            skill with a SKILL.md).
          </p>
          <form
            onsubmit={(event) => {
              event.preventDefault();
              install(repository);
            }}
          >
            <label class="worktree-input"
              >github.com/<input
                aria-label="Skill pack repository"
                placeholder="owner/repo"
                bind:value={repository}
                disabled={busy}
              /></label
            >
            <button disabled={busy || !repository.trim()}>Add</button>
          </form>
        </section>
        {#if preview}<section>
            <h2>Pack update</h2>
            {#if preview.up_to_date}<p>Already up to date.</p>{:else}
              <p>{preview.current_commit} → {preview.candidate_commit}</p>
              <pre>{preview.unified_diff}</pre>
              <button disabled={busy} onclick={confirm}
                >Update to reviewed commit</button
              >
            {/if}
            <button onclick={() => (preview = null)}>Close</button>
          </section>{/if}
      {:else}<p>
          {projectIdentifier === null
            ? 'Select a project to configure skills.'
            : 'Loading skills…'}
        </p>{/if}
    </div>
  </div>
</section>
