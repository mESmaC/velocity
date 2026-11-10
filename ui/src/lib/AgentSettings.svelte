<script lang="ts">
  import { onMount } from 'svelte';
  import {
    readAgentSettings,
    saveAgentSettings,
    type AgentSettings,
  } from './ipc';
  let settings = $state<AgentSettings | null>(null);
  let error = $state('');
  let saving = $state(false);
  onMount(() => {
    void readAgentSettings()
      .then((value) => {
        settings = value;
      })
      .catch((failure) => {
        error = String(failure);
      });
  });
  async function save() {
    if (!settings) return;
    saving = true;
    try {
      settings = await saveAgentSettings(settings);
      error = '';
    } catch (failure) {
      error = String(failure);
    } finally {
      saving = false;
    }
  }
</script>

<section aria-label="Agent settings">
  <h2>Agent activity and recovery</h2>
  <p class="worktree-description">
    Silent agents receive a nudge, then restart in their existing worktree.
    Parked agents and builds in the queue never count as stalled.
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
      >Max duration in minutes (empty means off)<span class="worktree-input"
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
      Failed agents and agents at a cap keep their worktrees. Open and closed
      PRs keep their claims.
    </p>
    <button
      class="worktree-button"
      disabled={saving}
      onclick={() => void save()}>{saving ? 'Saving...' : 'Save'}</button
    >
  {/if}
  {#if error}<p class="worktree-message error" role="alert">{error}</p>{/if}
</section>
