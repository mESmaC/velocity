<script lang="ts">
  import type { UsageGroup } from './ipc';
  import './theme/usage.css';

  let { group }: { group: UsageGroup } = $props();
</script>

<table class="usage-compare token-usage-table" aria-label={group.name}>
  <thead>
    <tr>
      <th scope="col">{group.name}</th>
      <th scope="col">Input</th>
      <th scope="col">Cached</th>
      <th scope="col">Output</th>
      <th scope="col">Total</th>
    </tr>
  </thead>
  <tbody>
    {#each group.rows as row (row.key)}
      <tr>
        <th scope="row">
          <details>
            <summary>{row.label}</summary>
            <small>{row.calls} usage records</small>
            <ul aria-label="Estimated input by source">
              {#each row.sources as source (source.label)}
                <li>{source.label} <span>{source.tokens}</span></li>
              {:else}
                <li>No input tokens</li>
              {/each}
            </ul>
          </details>
          {#if row.detail}<small>{row.detail}</small>{/if}
        </th>
        <td>{row.input}</td>
        <td>{row.cached}</td>
        <td>{row.output}</td>
        <td>{row.total}</td>
      </tr>
    {/each}
  </tbody>
</table>
