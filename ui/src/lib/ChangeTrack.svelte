<script lang="ts">
  let {
    steps,
    label = 'Change progress',
  }: {
    steps: { label: string; state: string }[];
    label?: string;
  } = $props();
</script>

<div class="change-track" aria-label={label}>
  {#each steps as step, index (index)}
    {#if index}<span class="bar" class:done={step.state === 'done'}></span>{/if}
    <span
      class={`step ${step.state}`}
      aria-current={step.state === 'run' ? 'step' : undefined}
    >
      <i aria-hidden="true"></i><span>{step.label}</span>
    </span>
  {/each}
</div>
