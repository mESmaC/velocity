<script lang="ts">
  import type { LayoutAction } from './ipc';

  // The settings page list shared by every settings page (SPEC.md F13).
  let {
    current,
    providersLabel = '',
    send,
  }: {
    current: 'worktrees' | 'providers' | 'agent-settings' | 'proofs';
    providersLabel?: string;
    send: (action: LayoutAction) => void;
  } = $props();
</script>

<nav class="worktree-settings-nav" aria-label="Settings pages">
  <button
    class:on={current === 'worktrees'}
    aria-current={current === 'worktrees' ? 'page' : undefined}
    onclick={() => send({ Request: { SettingsTab: 'worktrees' } })}
    >Worktrees</button
  >
  <button
    class:on={current === 'providers'}
    aria-current={current === 'providers' ? 'page' : undefined}
    onclick={() => send({ Request: { SettingsTab: 'providers' } })}
    >Providers <span>{providersLabel}</span></button
  >
  <button disabled>Skills</button>
  <button
    class:on={current === 'agent-settings'}
    aria-current={current === 'agent-settings' ? 'page' : undefined}
    onclick={() => send({ Request: { SettingsTab: 'agent-settings' } })}
    >Agents</button
  >
  <button
    class:on={current === 'proofs'}
    aria-current={current === 'proofs' ? 'page' : undefined}
    onclick={() => send({ Request: { SettingsTab: 'proofs' } })}>Proofs</button
  >
</nav>
