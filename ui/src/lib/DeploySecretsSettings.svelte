<script lang="ts">
  import { SvelteMap } from 'svelte/reactivity';
  import PaneState from './PaneState.svelte';
  import {
    deployCommands,
    type DeploySecretStatus,
    type ProjectDeploySettings,
  } from './ipc';

  let { project }: { project: number } = $props();

  let settings = $state<ProjectDeploySettings | null | undefined>(undefined);
  let loadError = $state('');
  let actionError = $state('');
  let notice = $state('');
  let busy = $state(false);
  let newSecretName = $state('');
  let newSecretValue = $state('');
  const secretInputs = new SvelteMap<string, string>();

  const currentProject = $derived(project);

  function formatError(reason: unknown): string {
    return reason instanceof Error ? reason.message : String(reason);
  }

  async function load(target: number) {
    try {
      const loaded = await deployCommands.projectSettings(target);
      if (target === currentProject) {
        settings = loaded;
        loadError = '';
      }
    } catch (reason) {
      if (target === currentProject) {
        loadError = formatError(reason);
      }
    }
  }

  $effect(() => {
    const target = currentProject;
    settings = undefined;
    loadError = '';
    actionError = '';
    notice = '';
    newSecretName = '';
    newSecretValue = '';
    secretInputs.clear();
    void load(target);
  });

  async function saveExistingSecret(secret: DeploySecretStatus) {
    const raw = secretInputs.get(secret.name) ?? '';
    if (busy || !raw.trim()) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const updated = await deployCommands.setSecret(target, secret.name, raw);
      if (target === currentProject) {
        settings = updated;
        secretInputs.delete(secret.name);
        notice = secret.stored
          ? `Replaced ${secret.name} in Windows Credential Manager.`
          : `Saved ${secret.name} in Windows Credential Manager.`;
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function clearSecret(name: string) {
    if (busy) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const updated = await deployCommands.clearSecret(target, name);
      if (target === currentProject) {
        settings = updated;
        secretInputs.delete(name);
        notice = `Removed ${name} from Windows Credential Manager.`;
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }

  async function addCustomSecret() {
    const name = newSecretName.trim();
    const value = newSecretValue;
    if (busy || !name || !value.trim()) return;
    const target = currentProject;
    busy = true;
    actionError = '';
    notice = '';
    try {
      const updated = await deployCommands.setSecret(target, name, value);
      if (target === currentProject) {
        settings = updated;
        newSecretName = '';
        newSecretValue = '';
        notice = `Saved ${name} in Windows Credential Manager.`;
      }
    } catch (reason) {
      if (target === currentProject) actionError = formatError(reason);
    } finally {
      if (target === currentProject) busy = false;
    }
  }
</script>

{#if settings !== null}
  <section aria-label="Deploy secrets">
    <h2>Deploy secrets</h2>
    <p class="worktree-description">
      Secrets for <code>rosen.toml</code> <code>[deploy]</code> steps are stored
      in Windows Credential Manager under
      <code>velocity/deploy/&lt;project&gt;/&lt;NAME&gt;</code>, passed only to
      matching deploy commands, and never shown again or exposed to agents. When
      <code>FLY_API_TOKEN</code> or <code>CLOUDFLARE_API_TOKEN</code> is not
      stored here, Velocity falls back to your local <code>flyctl</code> or
      <code>wrangler</code> login; save a token here if a step needs extra scopes.
    </p>
    <PaneState
      name="Deploy secrets"
      rosen
      loading={settings === undefined}
      error={loadError}
      retry={() => load(currentProject)}
      automatic={false}
    />
    {#if settings}
      {#if settings.config_error}
        <p class="worktree-message error" role="alert">
          {settings.config_error}
        </p>
      {/if}
      {#if !settings.supported}
        <p class="worktree-message" role="status">
          Deploy secret storage requires Windows Credential Manager.
        </p>
      {/if}

      {#if settings.steps.length > 0}
        <div class="worktree-setting-row deploy-block">
          <span>Configured deploy steps</span>
          <div class="deploy-step-list">
            {#each settings.steps as step (step.name)}
              <div class="deploy-step-row">
                <div class="deploy-step-head">
                  <code>{step.name}</code>
                  <span class="hint">{step.command}</span>
                </div>
                {#if step.secrets.length > 0}
                  <span class="hint">
                    Secrets: {step.secrets.join(', ')}
                  </span>
                {/if}
              </div>
            {/each}
          </div>
        </div>
      {/if}

      {#if settings.secrets.length > 0}
        <div class="worktree-setting-row deploy-block">
          <span>Stored secrets</span>
          <div class="deploy-secret-list">
            {#each settings.secrets as secret (secret.name)}
              <div class="deploy-secret-item">
                <div class="deploy-secret-meta">
                  <div class="deploy-secret-title">
                    <code>{secret.name}</code>
                    <span
                      class:deploy-status-ok={secret.stored ||
                        Boolean(secret.fallback)}
                      class:deploy-status-missing={!secret.stored &&
                        !secret.fallback &&
                        secret.used_by.length > 0}
                      class:hint={!secret.stored &&
                        !secret.fallback &&
                        secret.used_by.length === 0}
                    >
                      {secret.stored
                        ? 'Stored'
                        : secret.fallback
                          ? `Fallback (${secret.fallback})`
                          : 'Not set'}
                    </span>
                  </div>
                  <span class="hint">
                    {secret.target}
                    {#if secret.used_by.length > 0}
                      · used by {secret.used_by.join(', ')}
                    {/if}
                  </span>
                </div>
                <form
                  class="deploy-secret-form"
                  onsubmit={(event) => {
                    event.preventDefault();
                    void saveExistingSecret(secret);
                  }}
                >
                  <input
                    type="password"
                    aria-label={secret.stored
                      ? `Replace value for ${secret.name}`
                      : `Set value for ${secret.name}`}
                    placeholder={secret.stored
                      ? 'Enter new value to replace'
                      : secret.fallback
                        ? 'Enter token to override CLI login'
                        : 'Enter secret value'}
                    value={secretInputs.get(secret.name) ?? ''}
                    oninput={(event) =>
                      secretInputs.set(secret.name, event.currentTarget.value)}
                    disabled={busy || !settings.supported}
                    autocomplete="off"
                  />
                  <button
                    type="submit"
                    class="worktree-button mini"
                    disabled={busy ||
                      !settings.supported ||
                      !(secretInputs.get(secret.name) ?? '').trim()}
                    >{secret.stored ? 'Replace' : 'Save'}</button
                  >
                  {#if secret.stored}
                    <button
                      type="button"
                      class="worktree-button mini"
                      disabled={busy || !settings.supported}
                      onclick={() => void clearSecret(secret.name)}
                      >Clear</button
                    >
                  {/if}
                </form>
              </div>
            {/each}
          </div>
        </div>
      {/if}

      <div class="worktree-setting-row deploy-block">
        <span>Add secret</span>
        <form
          class="deploy-secret-form"
          onsubmit={(event) => {
            event.preventDefault();
            void addCustomSecret();
          }}
        >
          <input
            type="text"
            aria-label="Deploy secret name"
            placeholder="SECRET_NAME"
            spellcheck="false"
            bind:value={newSecretName}
            disabled={busy || !settings.supported}
            autocomplete="off"
          />
          <input
            type="password"
            aria-label="Deploy secret value"
            placeholder="Secret value (never shown again)"
            bind:value={newSecretValue}
            disabled={busy || !settings.supported}
            autocomplete="off"
          />
          <button
            type="submit"
            class="worktree-button mini"
            disabled={busy ||
              !settings.supported ||
              !newSecretName.trim() ||
              !newSecretValue.trim()}>Save secret</button
          >
        </form>
      </div>

      {#if actionError}
        <p class="worktree-message error" role="alert">{actionError}</p>
      {:else if notice}
        <p class="worktree-message" role="status">{notice}</p>
      {/if}
    {/if}
  </section>
{/if}

<style>
  .deploy-block {
    flex-direction: column;
    align-items: stretch;
    gap: var(--sp-3);
  }
  .deploy-step-list,
  .deploy-secret-list {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .deploy-step-row,
  .deploy-secret-item {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-4);
    border: 1px solid var(--line-dim);
    border-radius: var(--r-sm);
    background: var(--card);
  }
  .deploy-step-head,
  .deploy-secret-title {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    flex-wrap: wrap;
  }
  .deploy-secret-meta {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .deploy-secret-form {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    flex-wrap: wrap;
  }
  .deploy-secret-form input {
    flex: 1 1 180px;
    min-width: 0;
    height: var(--h-md);
    padding: 0 var(--sp-3);
    border: 1px solid var(--line-dim);
    border-radius: var(--r-sm);
    background: var(--field);
    color: var(--text);
    font-size: var(--fs-sm);
  }
  .deploy-status-ok {
    color: var(--ok);
    font-size: var(--fs-xs);
    font-weight: var(--fw-medium);
  }
  .deploy-status-missing {
    color: var(--warn);
    font-size: var(--fs-xs);
    font-weight: var(--fw-medium);
  }
</style>
