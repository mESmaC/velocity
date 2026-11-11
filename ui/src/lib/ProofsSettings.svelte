<script lang="ts">
  import { onMount } from 'svelte';
  import SettingsNav from './SettingsNav.svelte';
  import './theme/worktrees.css';
  import './theme/providers.css';
  import './theme/thread-view.css';
  import {
    proofsSettingsCommands,
    saveProofSettings,
    type LayoutAction,
    type ProofsSettingsView,
  } from './ipc';

  // Settings → Proofs (SPEC.md F13, F17) for the current project.
  let {
    project,
    send,
  }: { project: number; send: (action: LayoutAction) => void } = $props();
  let view = $state<ProofsSettingsView | null>(null);
  let inputs = $state(10_000);
  let error = $state('');
  let saving = $state(false);
  let saved = $state(false);
  const levelLabels = {
    Code: 'code',
    Reference: 'reference',
    Model: 'model',
  } as const;

  onMount(() => {
    void proofsSettingsCommands
      .read(project)
      .then((value) => {
        view = value;
        inputs = value.stress_diff_input_count;
      })
      .catch((failure: unknown) => {
        error = String(failure);
      });
  });

  async function save() {
    saving = true;
    saved = false;
    try {
      inputs = (await saveProofSettings({ stress_diff_input_count: inputs }))
        .stress_diff_input_count;
      error = '';
      saved = true;
    } catch (failure) {
      error = String(failure);
    } finally {
      saving = false;
    }
  }
</script>

<section class="worktree-settings" aria-label="Proof settings">
  <header class="worktree-settings-head">
    <h1>Settings</h1>
    <span>proof list and design proofs of this project</span>
  </header>
  <div class="worktree-settings-body">
    <SettingsNav current="proofs" {send} />
    <div class="worktree-settings-main">
      {#if view}
        <section>
          <h2>Proof list</h2>
          <p class="worktree-description">
            Only these features are proven in Lean, after their code. Every
            other feature ships with no Lean. SPEC.md records the list; adding a
            feature takes your decision there.
          </p>
          {#if !view.spec_found}
            <p class="worktree-size-note">This project has no SPEC.md yet.</p>
          {:else if !view.proof_list_found}
            <p class="worktree-size-note">
              SPEC.md has no proof list. Locking the spec interview writes one.
            </p>
          {:else if view.proof_list.length === 0}
            <p class="worktree-size-note">
              The proof list is empty: nothing in this project ships with Lean.
            </p>
          {:else}
            <div class="proof">
              <div class="thm">
                {#each view.proof_list as entry, index (index)}
                  <span class="k">Feature</span><code
                    ><b>{entry.feature}</b>{entry.detail
                      ? ` · ${entry.detail}`
                      : ''}</code
                  >
                {/each}
              </div>
            </div>
          {/if}
        </section>
        <section>
          <h2>Design proofs</h2>
          <p class="worktree-description">
            Each design proof in the Lake project, and how firmly each
            language's code is tied to its model. A model-only level where a
            stronger one is available shows in amber.
          </p>
          {#if view.lake_project === null}
            <p class="worktree-size-note">This project has no Lake project.</p>
          {:else if view.design_proofs.length === 0}
            <p class="worktree-size-note">
              No design proofs in <code>{view.lake_project}/</code> yet.
            </p>
          {:else}
            <div class="pv-list">
              {#each view.design_proofs as design (design.folder)}
                <div class="proof">
                  <div class="ph">
                    <span class="q" aria-hidden="true">∎</span>
                    <span><code>{design.folder}/Design.lean</code></span>
                  </div>
                  <div class="lv">
                    {#each design.levels as level, index (index)}
                      <span class="k">{level.language}</span><span
                        class={level.level}
                        class:flagged={level.flagged}
                        >{levelLabels[level.level]} · {level.detail}</span
                      >
                    {/each}
                  </div>
                </div>
              {/each}
            </div>
          {/if}
        </section>
        <section>
          <h2>Code-level translators</h2>
          <p class="worktree-description">
            A translator turns a language's code into Lean, so its confidence
            proof can reach the code level when an issue asks for one. Every
            other language tops out at the reference level: a fixture table
            checked by one test.
          </p>
          <div class="proof">
            <div class="lv">
              {#each view.translators as row (row.language)}
                <span class="k">{row.language}</span><span class={row.strongest}
                  >{row.translator ?? 'no translator'} · {levelLabels[
                    row.strongest
                  ]}</span
                >
              {/each}
            </div>
          </div>
        </section>
        <section>
          <h2>Stress diffs</h2>
          <p class="worktree-description">
            Scouts run the code and the compiled Lean model on the same
            generated inputs and file every disagreement with its input.
          </p>
          <label class="worktree-setting-row"
            >Inputs per feature<span class="worktree-input"
              ><input type="number" min="1" bind:value={inputs} /></span
            ></label
          >
          <button
            class="worktree-button"
            disabled={saving || !(inputs >= 1)}
            onclick={() => void save()}>{saving ? 'Saving...' : 'Save'}</button
          >
          {#if saved}<p class="worktree-message" role="status">
              Saved. The next sweep uses it.
            </p>{/if}
        </section>
      {/if}
      {#if error}<p class="worktree-message error" role="alert">
          {error}
        </p>{/if}
    </div>
  </div>
</section>
