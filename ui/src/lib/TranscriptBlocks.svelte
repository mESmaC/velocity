<script lang="ts">
  import Icon from './Icon.svelte';
  import type { DiffLineRole, TranscriptBlock } from './ipc';
  let { blocks }: { blocks: TranscriptBlock[] } = $props();

  const diffLineClasses: Record<DiffLineRole, string> = {
    Added: 'a',
    Deleted: 'd',
    Context: '',
    HunkHeader: 'c',
  };
</script>

{#each blocks as block, blockIndex (blockIndex)}
  {#if block.UserMessage}
    <div class="msg-user">{block.UserMessage.text}</div>
    {#if block.UserMessage.queued}
      <div class="queued">queued · sends when the current step finishes</div>
    {/if}
  {:else if block.AgentMessage}
    <div class="msg-agent">
      {#each block.AgentMessage.paragraphs as paragraph, paragraphIndex (paragraphIndex)}
        <p>{paragraph}</p>
      {/each}
    </div>
  {:else if block.ToolGroup}
    <div class="tools">
      {#each block.ToolGroup.steps as step, stepIndex (stepIndex)}
        <div class="tool" class:running={step.running}>
          {#if step.running}
            <span class="working-dots" aria-hidden="true"
              ><i></i><i></i><i></i></span
            >
          {:else}
            <span class="ok">✓</span>
          {/if}
          <span class="verb">{step.verb}</span>
          <span class="arg">{step.argument}</span>
          <span class="res">{step.result_text}</span>
        </div>
      {/each}
    </div>
  {:else if block.Diff}
    <div class="diff">
      <div class="diff-h">
        <span>{block.Diff.block.file_path}</span>
        <span>
          <span class="add">+{block.Diff.block.added}</span>
          <span class="del">−{block.Diff.block.deleted}</span>
        </span>
      </div>
      <pre>{#each block.Diff.block.lines as line, lineIndex (lineIndex)}<span
            class={diffLineClasses[line.role]}>{line.text}</span
          >{/each}</pre>
    </div>
  {:else if block.StatusLine}
    <div class="status-line">
      {#if block.StatusLine.running}
        <span class="working-dots" aria-hidden="true"
          ><i></i><i></i><i></i></span
        >
      {/if}
      <span>{block.StatusLine.text}</span>
    </div>
  {:else if block.ErrorCard}
    <div class="err-card" role="alert">
      <div class="ah">
        <Icon name="alert" />{block.ErrorCard.card.headline}
      </div>
      <pre>{block.ErrorCard.card.detail}</pre>
    </div>
  {:else if block.Decision}
    <div class="qa">
      <div class="q">{block.Decision.question}</div>
      <div class="d">
        ✓ <b>{block.Decision.key}</b><span>{block.Decision.value}</span>
      </div>
    </div>
  {/if}
{/each}
