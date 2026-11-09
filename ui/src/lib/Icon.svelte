<script lang="ts">
  let { name }: { name: string } = $props();
  const paths: Record<string, string> = {
    search: 'M10.5 10.5 14 14M11.5 7a4.5 4.5 0 1 1-9 0 4.5 4.5 0 0 1 9 0',
    new: 'M13 9v3.5a1 1 0 0 1-1 1H3.5a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1H7M11.5 2.5l2 2L8 10H6V8z',
    settings:
      'M10.2 8a2.2 2.2 0 1 1-4.4 0 2.2 2.2 0 0 1 4.4 0M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4',
    folder: 'M1.5 4h5l1.5 1.5h6.5v7H1.5zM1.5 5V3h5l1.5 1',
    plus: 'M8 3v10M3 8h10',
    chevron: 'M4 6l4 4 4-4',
    circle: 'M13.5 8a5.5 5.5 0 1 1-11 0 5.5 5.5 0 0 1 11 0',
    check: 'M3 8l3 3 7-7',
    play: 'M5 3l8 5-8 5z',
    swarm:
      'M8 1.5v4M2.5 12l3.5-2M13.5 12 10 10M10.5 8a2.5 2.5 0 1 1-5 0 2.5 2.5 0 0 1 5 0',
    pr: 'M4 6v7M12 10V5a2 2 0 0 0-2-2H8M8 1l-2 2 2 2M5.5 3.5a1.5 1.5 0 1 1-3 0 1.5 1.5 0 0 1 3 0M13.5 12a1.5 1.5 0 1 1-3 0 1.5 1.5 0 0 1 3 0',
    issue: 'M8 4.5v4M8 11h.01M14 8A6 6 0 1 1 2 8a6 6 0 0 1 12 0',
    agents: 'M3 2.5h7l3 3v8H3zM10 2.5v3h3M5 8h6M5 10.5h4',
    arrow: 'M8 12V3M4 7l4-4 4 4',
    airc: 'M2 3h12v8H7l-4 3v-3H2zM5 6h6M5 8h4',
    alert: 'M8 2 1.5 13h13zM8 6v3M8 11h.01',
    scout: 'M2 3h12v7H2zM5 10v3M11 10v3M5 6h.01M11 6h.01',
    usage: 'M3 13V8M8 13V3M13 13V6',
    update: 'M13 5a5.5 5.5 0 1 0 .5 5M10 2l3 3-3 3',
    goal: 'M14 8A6 6 0 1 1 2 8a6 6 0 0 1 12 0M11 8a3 3 0 1 1-6 0 3 3 0 0 1 6 0M8 8h.01',
  };
</script>

<svg class="icon" viewBox="0 0 16 16" aria-hidden="true"
  ><path d={paths[name] ?? paths.circle} /></svg
>
