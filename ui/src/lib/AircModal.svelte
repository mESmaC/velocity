<script lang="ts">
  import PaneState from './PaneState.svelte';
  import { onMount, tick } from 'svelte';
  import { pop } from './motion';
  import {
    aircCommands,
    aircRoutingCommands,
    aircModalCommands,
    onAircModal,
    pullRequestCommands,
  } from './ipc';
  import type {
    AircLineView,
    AircModalView,
    AircTab,
    LayoutAction,
    LayoutSnapshot,
    NickTone,
    MessageMode,
  } from './ipc';
  import './theme/airc.css';

  let {
    project,
    send,
    onResult,
  }: {
    project: number;
    send: (action: LayoutAction) => void;
    onResult: (snapshot: LayoutSnapshot) => void;
  } = $props();

  let dialog: HTMLDialogElement;
  let input: HTMLInputElement;
  let main: HTMLElement | undefined = $state();
  let view = $state<AircModalView | null>(null);
  let tab = $state<AircTab>('Events');
  let text = $state('');
  let error = $state('');
  let undoing = $state(false);
  // Keep a log pinned to its newest line unless you scrolled up to read.
  let pinned = true;

  const lines = $derived<AircLineView[]>(
    !view
      ? []
      : tab === 'Events'
        ? view.events
        : tab === 'Messages'
          ? view.messages
          : tab === 'Discoveries'
            ? view.discoveries
            : view.past_work,
  );

  onMount(() => {
    const previous = document.activeElement;
    let active = true;
    let stop: (() => void) | null = null;
    dialog.showModal();
    input.focus();
    void onAircModal((event) => {
      if (event.payload.project_identifier === project) {
        view = event.payload.view;
        error = '';
      }
    })
      .then((unlisten) => {
        if (active) stop = unlisten;
        else unlisten();
      })
      .catch(report);
    aircModalCommands
      .view(project)
      .then((next) => (view ??= next))
      .catch(report);
    return () => {
      active = false;
      stop?.();
      if (previous instanceof HTMLElement && previous.isConnected)
        previous.focus();
    };
  });

  $effect(() => {
    void lines;
    if (pinned)
      void tick().then(() => main?.scrollTo({ top: main.scrollHeight }));
  });

  async function refresh() {
    try {
      view = await aircModalCommands.view(project);
      error = '';
    } catch (reason) {
      report(reason);
    }
  }

  function report(reason: unknown) {
    error = reason instanceof Error ? reason.message : String(reason);
  }

  function dismiss() {
    send({ Request: { Navigation: 'Dismiss' } });
  }

  function choose(next: AircTab) {
    tab = next;
    pinned = true;
  }

  function scrolled() {
    if (main)
      pinned = main.scrollHeight - main.scrollTop - main.clientHeight < 40;
  }

  function mention(nick: string) {
    text = `${nick}: `;
    input.focus();
  }

  function setMode(nick: string, mode: MessageMode) {
    void aircRoutingCommands.setMode(project, nick, mode).catch(report);
  }

  function comment() {
    const line = text.trim();
    if (!line) return;
    error = '';
    aircCommands
      .send(project, line)
      .then(() => {
        text = '';
        choose('Messages');
      })
      .catch(report);
  }

  function openPullRequest(number: number) {
    pullRequestCommands
      .open(project, number)
      .then((snapshot) => {
        if (snapshot) onResult(snapshot);
        dismiss();
      })
      .catch(report);
  }

  async function undoTaskEvent(request: string) {
    error = '';
    undoing = true;
    try {
      await aircModalCommands.undoTaskEvent(project, request);
    } catch (reason) {
      report(reason);
    } finally {
      undoing = false;
    }
  }

  function tone(value: NickTone): string {
    return typeof value === 'string' ? `nick-${value}` : `nick-${value.Hue}`;
  }
</script>

<dialog
  bind:this={dialog}
  in:pop|global
  class="airc-dialog"
  aria-labelledby="airc-title"
  oncancel={(event) => {
    event.preventDefault();
    dismiss();
  }}
  onclick={(event) => {
    if (event.target === dialog) dismiss();
  }}
>
  <div class="airc">
    <div class="airc-h">
      <h2 id="airc-title">AIRC</h2>
      <span class="chan">{view?.channel ?? ''}</span><span class="topic"
        >claims and project events · agents message each other only when
        coordination needs it</span
      ><span class="label">Esc to close</span>
    </div>
    <div class="airc-tabs" role="tablist">
      {#each view?.tabs ?? [] as entry (entry.tab)}<button
          role="tab"
          aria-selected={tab === entry.tab}
          class:on={tab === entry.tab}
          onclick={() => choose(entry.tab)}
          >{entry.label}<span class:warn={entry.warn}>{entry.count}</span
          ></button
        >{/each}
    </div>
    <div class="airc-body">
      <div class="airc-main" bind:this={main} onscroll={scrolled}>
        <PaneState
          name={`AIRC ${tab}`}
          rosen
          loading={!view}
          {error}
          retry={refresh}
        />
        {#if view && tab === 'Claims'}
          <p class="cl-note" class:warn={view?.claims_warn}>
            {view?.claims_note ?? ''}
          </p>
          {#each view?.claims ?? [] as claim, index (`${index}:${claim.nick}:${claim.task}`)}<div
              class="cl"
            >
              <span class="n {tone(claim.nick_tone)}">{claim.nick}</span><span
                class="tk">{claim.task}</span
              ><span class="st">{claim.state}</span><span class="fs"
                >{#each claim.files as file, index (`${index}:${file.path}`)}<code
                    class:ov={file.overlap}>{file.path}</code
                  >{/each}</span
              >
              {#if claim.dependency}<span class="dep">{claim.dependency}</span
                >{/if}
            </div>{:else}<PaneState
              name="Claims"
              empty
              message="No active claims."
            />{/each}
        {:else if view && (tab === 'Discoveries' || tab === 'PastWork')}
          {#each lines as line (line.key)}<div class="lg">
              <span class="ts">{line.time}</span><span
                class="n {tone(line.nick_tone)}">{line.nick}</span
              ><span class="x">{@render spans(line)}</span>
            </div>{:else}<PaneState
              name={tab}
              empty
              message="Nothing logged yet."
            />{/each}
        {:else if view}
          {#each lines as line (line.key)}{#if tab === 'Events'}<div
                class="al ev tone-{line.tone}"
                class:overlap={line.tag === 'overlap'}
              >
                <span class="ts">{line.time}</span><span class="n"
                  ><i class="etag">{line.tag}</i></span
                ><span class="x"
                  >{#if line.nick}<b class={tone(line.nick_tone)}>{line.nick}</b
                    >
                  {/if}{@render spans(line)}
                  {#if typeof line.task_undo_request === 'string'}
                    {@const request = line.task_undo_request}
                    <button
                      class="btn"
                      type="button"
                      disabled={undoing}
                      onclick={() => void undoTaskEvent(request)}>Undo</button
                    >
                  {/if}</span
                >
              </div>{:else}<div
                class="al"
                class:you={line.from_you}
                class:ment={line.mention}
              >
                <span class="ts">{line.time}</span><span
                  class="n {tone(line.nick_tone)}">&lt;{line.nick}&gt;</span
                ><span class="x">{@render spans(line)}</span>
              </div>{/if}{:else}<PaneState
              name={tab}
              empty
              message={tab === 'Events'
                ? 'No events yet · Project activity appears here.'
                : 'No messages yet · Coordination messages appear here.'}
            />{/each}
        {/if}
      </div>
      <aside class="airc-nicks" aria-label="Nick list">
        {#each view?.nick_groups ?? [] as group (group.label)}<div class="nk-h">
            {group.label}
          </div>
          {#each group.rows as row, index (`${index}:${row.nick}`)}<button
              class="nk"
              title="Mention {row.nick}"
              onclick={() => mention(row.nick)}
              ><i class={row.dot}></i><b class={tone(row.tone)}>{row.nick}</b
              ><small>{row.detail}</small></button
            >{#if row.dot !== 'Left'}
              <select
                aria-label="Message mode for {row.nick}"
                value={row.message_mode}
                onchange={(event) =>
                  setMode(row.nick, event.currentTarget.value as MessageMode)}
              >
                <option value="Open">Open</option><option value="HeadsDown"
                  >Heads-down</option
                >
                <option value="Quarantined">Quarantined</option>
              </select>
            {/if}{/each}{/each}
      </aside>
    </div>
    <form
      class="airc-in"
      onsubmit={(event) => {
        event.preventDefault();
        comment();
      }}
    >
      <span class="me">&lt;you&gt;</span><input
        bind:this={input}
        bind:value={text}
        aria-label="Comment"
        spellcheck="false"
        placeholder="Comment for every agent, or “nick: …” to reach one. They read it at their next step."
      />
    </form>
  </div>
</dialog>

{#snippet spans(
  line: AircLineView,
)}{#each line.spans as span, index (index)}{#if 'Text' in span}{span.Text}{:else if 'Nick' in span && span.Nick}<b
        class={tone(span.Nick.tone)}>{span.Nick.nick}</b
      >{:else if 'PullRequest' in span && span.PullRequest !== undefined}<button
        class="ref"
        onclick={() => openPullRequest(span.PullRequest ?? 0)}
        >#{span.PullRequest}</button
      >{/if}{/each}{/snippet}
