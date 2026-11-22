<script lang="ts">
  import { onMount } from 'svelte';
  import SettingsNav from './SettingsNav.svelte';
  import SharedMcpServers from './SharedMcpServers.svelte';
  import './theme/providers.css';
  import './theme/workspaces.css';
  import {
    localBuildCommands,
    onLocalBuildStatus,
    readAgentSettings,
    saveAgentSettings,
    type AgentPreferences,
    type EffortLevel,
    type LayoutAction,
  } from './ipc';
  let { send }: { send: (action: LayoutAction) => void } = $props();
  let preferences = $state<AgentPreferences | null>(null);
  let settings = $derived(preferences?.agents);
  let error = $state('');
  let saving = $state(false);
  let autoRestartLocalBuild = $state(true);
  let savingAutoRestart = $state(false);
  const efforts: [EffortLevel, string][] = [
    ['low', 'Low'],
    ['medium', 'Medium'],
    ['high', 'High'],
    ['max', 'Max'],
  ];
  onMount(() => {
    void readAgentSettings()
      .then((value) => {
        preferences = value;
      })
      .catch((failure) => {
        error = String(failure);
      });
    let stopped = false;
    void localBuildCommands
      .status()
      .then((view) => {
        if (!stopped) autoRestartLocalBuild = view.auto_restart;
      })
      .catch(() => {});
    const listening = onLocalBuildStatus((event) => {
      autoRestartLocalBuild = event.payload.view.auto_restart;
    });
    return () => {
      stopped = true;
      void listening.then((unlisten) => unlisten());
    };
  });
  async function toggleAutoRestart(enabled: boolean) {
    if (savingAutoRestart) return;
    savingAutoRestart = true;
    try {
      const view = await localBuildCommands.setAutoRestart(enabled);
      autoRestartLocalBuild = view.auto_restart;
      error = '';
    } catch (failure) {
      error = String(failure);
    } finally {
      savingAutoRestart = false;
    }
  }
  async function save() {
    if (!preferences || saving) return;
    saving = true;
    try {
      preferences = await saveAgentSettings(preferences);
      error = '';
    } catch (failure) {
      error = String(failure);
    } finally {
      saving = false;
    }
  }
</script>

{#snippet effortRow(
  label: string,
  value: EffortLevel | null,
  change: (effort: EffortLevel) => void,
  reset?: () => void,
)}
  <div class="workspace-setting-row">
    <span>{label}</span>
    <div class="pv-seg" role="group" aria-label={label}>
      {#if reset}
        <button
          type="button"
          class:on={value === null}
          aria-pressed={value === null}
          disabled={saving}
          onclick={reset}>Auto</button
        >
      {/if}
      {#each efforts as [level, name] (level)}
        <button
          type="button"
          class:on={value === level}
          aria-pressed={value === level}
          disabled={saving}
          onclick={() => change(level)}>{name}</button
        >
      {/each}
    </div>
  </div>
{/snippet}

<section class="workspace-settings" aria-label="Agent settings">
  <header class="workspace-settings-head">
    <h1>Settings</h1>
    <span>applies to every project</span>
  </header>
  <div class="workspace-settings-body">
    <SettingsNav current="agent-settings" {send} />
    <div class="workspace-settings-main">
      {#if preferences}
        {@const roles = preferences}
        <section>
          <h2>Agent browser</h2>
          <label class="workspace-setting-row">
            <span>Concurrent browsers</span>
            <input
              type="number"
              min="1"
              max="16"
              value={roles.agents.browser_limit}
              disabled={saving}
              onchange={(event) => {
                const limit = event.currentTarget.valueAsNumber;
                if (Number.isInteger(limit) && limit >= 1 && limit <= 16)
                  roles.agents.browser_limit = limit;
                event.currentTarget.value = String(roles.agents.browser_limit);
              }}
            />
          </label>
        </section>
        <section>
          <h2>Reasoning effort</h2>
          {@render effortRow(
            'Implementation',
            roles.implementationEffort,
            (level) => {
              roles.implementationEffort = level;
            },
          )}
          {@render effortRow(
            'Review',
            roles.agents.review_effort,
            (level) => {
              roles.agents.review_effort = level;
            },
            () => {
              roles.agents.review_effort = null;
            },
          )}
          {@render effortRow(
            'Duty sweep',
            roles.agents.sweep_effort,
            (level) => {
              roles.agents.sweep_effort = level;
            },
          )}
          {@render effortRow('Title', roles.titleEffort, (level) => {
            roles.titleEffort = level;
          })}
          <p class="workspace-description">
            Auto uses Medium for trivial reading reviews and High for other
            reviews. An agent's explicit effort overrides its role default.
          </p>
        </section>
      {/if}
      <section>
        <h2>Agent activity and recovery</h2>
        <p class="workspace-description">
          Silent agents receive a nudge, then restart in their existing
          workspace. Parked agents and builds in the queue never count as
          stalled.
        </p>
        {#if settings}
          <label class="workspace-setting-row"
            >Max turns (empty means off)<span class="workspace-input"
              ><input
                type="number"
                min="1"
                value={settings.max_turns ?? ''}
                onchange={(event) => {
                  if (settings)
                    settings.max_turns = event.currentTarget.value
                      ? Number(event.currentTarget.value)
                      : null;
                }}
              /></span
            ></label
          >
          <label class="workspace-setting-row"
            >Max duration in minutes (empty means off)<span
              class="workspace-input"
              ><input
                type="number"
                min="1"
                value={settings.max_duration_minutes ?? ''}
                onchange={(event) => {
                  if (settings)
                    settings.max_duration_minutes = event.currentTarget.value
                      ? Number(event.currentTarget.value)
                      : null;
                }}
              /></span
            ></label
          >
          <label class="workspace-setting-row"
            >Stall timeout in minutes<span class="workspace-input"
              ><input
                type="number"
                min="1"
                bind:value={settings.stall_timeout_minutes}
              /></span
            ></label
          >
          <label class="workspace-setting-row"
            >Running tool timeout in minutes<span class="workspace-input"
              ><input
                type="number"
                min="1"
                bind:value={settings.tool_timeout_minutes}
              /></span
            ></label
          >
          <label class="workspace-setting-row"
            >Claim release grace in minutes<span class="workspace-input"
              ><input
                type="number"
                min="1"
                bind:value={settings.claim_release_grace_minutes}
              /></span
            ></label
          >
          <label class="workspace-setting-row">
            Experimental context hand-off
            <input type="checkbox" bind:checked={settings.context_handoff} />
          </label>
          <p class="workspace-description">
            Applies to new Rosen work items. At 120,000 context tokens, Velocity
            can carry the work into a fresh session. See Usage for the
            comparison.
          </p>
          <p class="workspace-description">
            Failed agents and agents at a cap keep their workspaces. Open and
            closed changes keep their claims.
          </p>
        {/if}
      </section>
      <section>
        {#if settings}
          <h2>Agent launch</h2>
          <label class="workspace-check">
            <input
              type="checkbox"
              role="switch"
              bind:checked={settings.trim_agent_context}
            />
            Trim unused agent context
          </label>
          <p class="workspace-description">
            New Claude Code and Codex sessions skip desktop integrations and
            provider skill context to use fewer tokens.
          </p>
          <button
            class="workspace-button"
            disabled={saving}
            onclick={() => void save()}>{saving ? 'Saving...' : 'Save'}</button
          >
        {/if}
        {#if error}<p class="workspace-message error" role="alert">
            {error}
          </p>{/if}
      </section>
      <section>
        <h2>Local builds</h2>
        <label class="workspace-check">
          <input
            type="checkbox"
            role="switch"
            checked={autoRestartLocalBuild}
            disabled={savingAutoRestart}
            onchange={(event) =>
              void toggleAutoRestart(event.currentTarget.checked)}
          />
          Auto-restart into local builds
        </label>
        <p class="workspace-description">
          Restart into the newest local build by itself when waiting issues need
          its agent tooling, or at the next idle check with no turn, land,
          deploy or local build in progress.
        </p>
      </section>
      <SharedMcpServers />
    </div>
  </div>
</section>
