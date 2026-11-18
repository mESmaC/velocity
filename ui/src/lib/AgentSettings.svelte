<script lang="ts">
  import { onMount } from 'svelte';
  import SettingsNav from './SettingsNav.svelte';
  import SharedMcpServers from './SharedMcpServers.svelte';
  import './theme/providers.css';
  import './theme/worktrees.css';
  import {
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
  });
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
  <div class="worktree-setting-row">
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

<section class="worktree-settings" aria-label="Agent settings">
  <header class="worktree-settings-head">
    <h1>Settings</h1>
    <span>applies to every project</span>
  </header>
  <div class="worktree-settings-body">
    <SettingsNav current="agent-settings" {send} />
    <div class="worktree-settings-main">
      {#if preferences}
        {@const roles = preferences}
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
          <p class="worktree-description">
            Auto uses Medium for trivial reading reviews and High for other
            reviews. An agent's explicit effort overrides its role default.
          </p>
        </section>
      {/if}
      <section>
        <h2>Agent activity and recovery</h2>
        <p class="worktree-description">
          Silent agents receive a nudge, then restart in their existing
          workspace. Parked agents and builds in the queue never count as
          stalled.
        </p>
        {#if settings}
          <label class="worktree-setting-row"
            >Max turns (empty means off)<span class="worktree-input"
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
          <label class="worktree-setting-row"
            >Max duration in minutes (empty means off)<span
              class="worktree-input"
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
          <label class="worktree-setting-row"
            >Stall timeout in minutes<span class="worktree-input"
              ><input
                type="number"
                min="1"
                bind:value={settings.stall_timeout_minutes}
              /></span
            ></label
          >
          <label class="worktree-setting-row"
            >Running tool timeout in minutes<span class="worktree-input"
              ><input
                type="number"
                min="1"
                bind:value={settings.tool_timeout_minutes}
              /></span
            ></label
          >
          <label class="worktree-setting-row"
            >Claim release grace in minutes<span class="worktree-input"
              ><input
                type="number"
                min="1"
                bind:value={settings.claim_release_grace_minutes}
              /></span
            ></label
          >
          <p class="worktree-description">
            Failed agents and agents at a cap keep their workspaces. Open and
            closed changes keep their claims.
          </p>
        {/if}
      </section>
      <section>
        {#if settings}
          <h2>Agent launch</h2>
          <label class="worktree-check">
            <input
              type="checkbox"
              role="switch"
              bind:checked={settings.trim_agent_context}
            />
            Trim unused agent context
          </label>
          <p class="worktree-description">
            New Claude Code and Codex sessions skip desktop integrations and
            provider skill context to use fewer tokens.
          </p>
          <button
            class="worktree-button"
            disabled={saving}
            onclick={() => void save()}>{saving ? 'Saving...' : 'Save'}</button
          >
        {/if}
        {#if error}<p class="worktree-message error" role="alert">
            {error}
          </p>{/if}
      </section>
      <SharedMcpServers />
    </div>
  </div>
</section>
