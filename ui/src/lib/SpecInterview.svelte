<script lang="ts">
  import Composer from './Composer.svelte';
  import Icon from './Icon.svelte';
  import TranscriptBlocks from './TranscriptBlocks.svelte';
  import type {
    InterviewView,
    LayoutAction,
    LayoutSnapshot,
    NavigationIntent,
    ThreadDetailSnapshot,
  } from './ipc';
  let {
    layout,
    thread,
    interview,
    send,
  }: {
    layout: LayoutSnapshot;
    thread: ThreadDetailSnapshot;
    interview: InterviewView;
    send: (action: LayoutAction) => void;
  } = $props();

  function request(intent: NavigationIntent) {
    send({ Request: { Navigation: intent } });
  }
</script>

<section class="thread-view spec-view" aria-label={thread.title}>
  <header class="thread-head">
    <h1>{thread.title}</h1>
    <div class="meta">
      {#if thread.branch}
        <span class="branch"><Icon name="branch" />{thread.branch}</span>
      {/if}
      <span>{interview.status_label}</span>
      {#if thread.model_pill.model_name}<span
          >{thread.model_pill.model_name}</span
        >{/if}
    </div>
    <span class="spacer"></span>
    <div class="head-actions">
      <button type="button" class="btn" onclick={() => request('SpecFile')}>
        SPEC.md <kbd>S</kbd>
      </button>
      <button
        type="button"
        class="btn"
        class:primary={interview.phase === 'Ready'}
        disabled={interview.phase !== 'Ready'}
        onclick={() => request('Lock')}
      >
        Lock spec <kbd>L</kbd>
      </button>
    </div>
  </header>

  <div class="transcript">
    <div class="transcript-inner">
      {#if thread.blocks.length}
        <TranscriptBlocks blocks={thread.blocks} />
      {:else}
        {#if interview.brief}<div class="msg-user">{interview.brief}</div>{/if}
        {#if interview.summary}<div class="msg-agent">
            <p>{interview.summary}</p>
          </div>{/if}
        {#each interview.decisions as decision, index (index)}
          <div class="qa">
            <div class="d">
              ✓ <b>{decision.key}</b><span>{decision.value}</span>
            </div>
          </div>
        {/each}
      {/if}

      {#if thread.agent_busy}
        <div class="status-line">
          <span class="working-dots" aria-hidden="true"
            ><i></i><i></i><i></i></span
          >
          <span
            >{interview.phase === 'Scan'
              ? 'looking through the project…'
              : 'updating the spec…'}</span
          >
        </div>
      {:else if interview.phase === 'Ask' && interview.question}
        {@const question = interview.question}
        <div class="q-card">
          <div class="qn">
            QUESTION {question.number} · {question.key.toUpperCase()}
          </div>
          <div class="qt">{question.text}</div>
          {#if question.why}<div class="why">{question.why}</div>{/if}
          <div class="opts">
            {#each question.options as option, index (index)}
              <button
                type="button"
                class="opt"
                class:rec={option.recommended}
                onclick={() => request({ Answer: String(index + 1) })}
              >
                <kbd>{index + 1}</kbd>
                <div>
                  <b>{option.label}</b>
                  {#if option.detail}<span>{option.detail}</span>{/if}
                </div>
              </button>
            {/each}
          </div>
          <div class="foot">or type your own answer below · Enter to send</div>
        </div>
      {:else if interview.phase === 'Ready'}
        <div class="spec-card">
          <div class="ah">
            <Icon name="agents" /> Spec ready · {interview.decisions.length} decisions
            · {interview.task_count} tasks drafted
          </div>
          <dl>
            {#each interview.decisions as decision, index (index)}
              <dt>{decision.key}</dt>
              <dd>{decision.value}</dd>
            {/each}
          </dl>
          <div class="ah">Proof list · proven in Lean after the code (F17)</div>
          <div class="checks">
            {#each interview.proof_list as entry, index (index)}
              <label class="check">
                <input
                  type="checkbox"
                  checked={entry.included}
                  onchange={() =>
                    send({ Request: { ToggleProofListEntry: String(index) } })}
                />
                {entry.feature}
              </label>
            {:else}
              <span class="check">No features proposed for proofs.</span>
            {/each}
          </div>
          <div class="acts">
            <button
              type="button"
              class="btn solid"
              onclick={() => request('Lock')}
            >
              Lock spec & publish tasks <kbd>L</kbd>
            </button>
            <button
              type="button"
              class="btn"
              onclick={() => request('SpecFile')}
            >
              Read SPEC.md <kbd>S</kbd>
            </button>
            <button type="button" class="btn" onclick={() => request('Grill')}>
              Keep grilling <kbd>G</kbd>
            </button>
          </div>
        </div>
      {:else if interview.phase === 'Locked'}
        <div class="spec-card locked">
          <div class="ah">
            <Icon name="check" /> Spec locked · SPEC.md committed · {interview.task_count}
            tasks published
          </div>
          <div class="msg-agent">
            <p>The Task List is now this project’s source of truth.</p>
          </div>
        </div>
      {/if}

      {#if interview.notice}<p class="spec-notice" role="status">
          {interview.notice}
        </p>{/if}
    </div>
  </div>

  <div class="dock">
    <Composer
      {layout}
      {send}
      id="thread-composer"
      placeholder={thread.composer_placeholder}
      modelPill={thread.model_pill}
    />
  </div>
</section>
