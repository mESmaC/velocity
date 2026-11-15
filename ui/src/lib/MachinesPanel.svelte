<script lang="ts">
  import { onMount } from 'svelte';
  import { machineCommands } from './ipc';
  import type { MachinesView, MachineRole } from './ipc';

  let { project }: { project: number } = $props();
  let view = $state<MachinesView | null>(null);
  let bindAddress = $state('0.0.0.0:7443');
  let advertisedAddress = $state('');
  let expires = $state(3600);
  let uses = $state(1);
  let role = $state<MachineRole>('agent');
  let key = $state('');
  let prompt = $state('');
  let busy = $state(false);
  let error = $state('');
  let notice = $state('');

  async function refresh() {
    const selected = project;
    const next = await machineCommands.list(selected);
    if (project === selected) view = next;
  }

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  async function act(action: () => Promise<unknown>) {
    busy = true;
    error = '';
    notice = '';
    try {
      await action();
      await refresh();
    } catch (reason) {
      report(reason);
    } finally {
      busy = false;
    }
  }

  async function createKey() {
    key = await machineCommands.createKey(
      project,
      advertisedAddress,
      expires,
      uses,
      role,
    );
  }

  async function dispatch(machine: string) {
    const sequence = await machineCommands.dispatch(project, machine, prompt);
    notice = `Dispatch ${sequence} queued on the machine.`;
  }

  function contact(seconds: string) {
    const time = Number(seconds);
    return time > 0 ? new Date(time * 1000).toLocaleString() : 'No contact yet';
  }

  onMount(() => {
    void refresh().catch(report);
    const timer = globalThis.setInterval(() => {
      if (!busy) void refresh().catch(report);
    }, 5000);
    return () => globalThis.clearInterval(timer);
  });
</script>

<section aria-label="Headless machines">
  <h2>Machines</h2>
  <p class="worktree-description">
    One join key enrolls a headless machine in Rosen and Velocity dispatch.
    Provider sign-ins and tools come from the machine's existing OS account.
  </p>
  <div class="worktree-setting-row">
    <label for="machine-listen">Listen address</label>
    <label class="worktree-input"
      ><input id="machine-listen" bind:value={bindAddress} /></label
    >
    <button
      class="worktree-button mini"
      disabled={busy}
      onclick={() =>
        void act(() => machineCommands.listen(project, bindAddress))}
      >Enable listener</button
    >
  </div>
  {#if view?.listener}<p class="worktree-description">
      Listening on {view.listener}
    </p>{/if}
  <div class="worktree-setting-row">
    <label for="machine-address">Address reachable from workers</label>
    <label class="worktree-input"
      ><input
        id="machine-address"
        placeholder="hostname:7443"
        bind:value={advertisedAddress}
      /></label
    >
  </div>
  <div class="worktree-setting-row">
    <label for="machine-expiry">Key expiry (seconds)</label>
    <label class="worktree-input"
      ><input
        id="machine-expiry"
        type="number"
        min="1"
        step="1"
        bind:value={expires}
      /></label
    >
    <label for="machine-uses">Uses</label>
    <label class="worktree-input"
      ><input
        id="machine-uses"
        type="number"
        min="1"
        step="1"
        bind:value={uses}
      /></label
    >
  </div>
  <div class="worktree-setting-row">
    <label class="worktree-check"
      ><input
        type="radio"
        name="machine-role"
        bind:group={role}
        value="agent"
      />Agent machine</label
    >
    <label class="worktree-check"
      ><input
        type="radio"
        name="machine-role"
        bind:group={role}
        value="checker"
      />Checker</label
    >
    <button
      class="worktree-button mini"
      disabled={busy || !view?.listener || !advertisedAddress.trim()}
      onclick={() => void act(createKey)}>Create join key</button
    >
  </div>
  {#if key}
    <p class="worktree-description">
      Run on the worker under its provisioned OS account:
    </p>
    <p class="worktree-path-preview">
      <code
        >velocity-app machine join --key {key} --data-dir "C:\VelocityWorker" --slots
        40</code
      >
    </p>
    <p class="worktree-description">
      This command keeps the worker running. To restart it, use <code
        >velocity-app machine run --data-dir "C:\VelocityWorker" --slots 40</code
      >. A service manager can run the same command.
    </p>
    <button class="worktree-button mini" onclick={() => (key = '')}
      >Hide key</button
    >
  {/if}
  <div class="worktree-setting-row">
    <label for="machine-prompt">Work to dispatch</label>
    <label class="worktree-input"
      ><input id="machine-prompt" bind:value={prompt} /></label
    >
  </div>
  {#each view?.machines ?? [] as machine (machine.id)}
    <div class="worktree-setting-row">
      <span>{machine.name} · {machine.role} · {machine.status}</span>
      <span class="hint"
        >{contact(machine.last_contact)} · {machine.running_agents.length} running</span
      >
      {#if machine.status === 'pending'}
        <button
          class="worktree-button mini"
          disabled={busy}
          onclick={() =>
            void act(() => machineCommands.approve(project, machine.id))}
          >Approve</button
        >
      {/if}
      {#if machine.status === 'active'}
        <button
          class="worktree-button mini"
          disabled={busy || !machine.dispatch_ready || !prompt.trim()}
          onclick={() => void act(() => dispatch(machine.id))}>Dispatch</button
        >
      {/if}
      {#if machine.status !== 'revoked'}
        <button
          class="worktree-button mini"
          disabled={busy}
          onclick={() =>
            void act(() => machineCommands.revoke(project, machine.id))}
          >Revoke</button
        >
      {/if}
    </div>
    {#if machine.running_agents.length}
      <p class="worktree-description">{machine.running_agents.join(', ')}</p>
    {/if}
  {/each}
  {#if view && !view.machines.length}<p class="worktree-description">
      No machines enrolled.
    </p>{/if}
  {#each view?.recent_dispatch ?? [] as result, index (index)}<p
      class="worktree-description"
    >
      {result}
    </p>{/each}
  {#if error}<p class="worktree-message error" role="alert">{error}</p>{/if}
  {#if notice}<p class="worktree-message" role="status">{notice}</p>{/if}
</section>
