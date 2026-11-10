<script lang="ts" module>
  /** Puts the caret `offset` characters into `node`, across lint marks and code. */
  export function placeCaret(node: HTMLElement, offset: number) {
    const selection = document.getSelection();
    if (!selection) return;
    const range = document.createRange();
    const walker = document.createTreeWalker(
      node,
      globalThis.NodeFilter.SHOW_TEXT,
    );
    let remaining = offset;
    for (let text = walker.nextNode(); text; text = walker.nextNode()) {
      const length = text.textContent?.length ?? 0;
      if (remaining <= length) {
        range.setStart(text, remaining);
        range.collapse(true);
        selection.removeAllRanges();
        selection.addRange(range);
        return;
      }
      remaining -= length;
    }
    range.selectNodeContents(node);
    range.collapse(false);
    selection.removeAllRanges();
    selection.addRange(range);
  }
</script>

<script lang="ts">
  import type {
    InlineText,
    LintKind,
    OutlineAction,
    OutlineLine,
    RewriteView,
  } from './ipc';

  let {
    line,
    send,
    submit,
    rewrite,
    pending,
    flagged,
  }: {
    line: OutlineLine;
    send: (action: OutlineAction, caret?: number) => void;
    submit: (identifier: number) => void;
    rewrite: RewriteView | undefined;
    pending: boolean;
    flagged: boolean;
  } = $props();

  let changed = false;
  function leave() {
    if (changed && line.kind === 'Rule') submit(line.identifier);
    changed = false;
  }

  // While typing, the browser's own edit stands: a snapshot repaints the lint marks only when
  // it already holds the typed text, and keeps the caret where it was.
  function editable(node: HTMLElement, value: OutlineLine) {
    let current = value;
    let composing = false;
    function render() {
      const focused = document.activeElement === node;
      if (focused && (composing || node.textContent !== current.text)) return;
      const offset = focused ? caretOffset(node) : 0;
      node.replaceChildren();
      if (current.kind !== 'Rule') node.textContent = current.text;
      else renderInline(node, current.inline, focused);
      if (focused) placeCaret(node, offset);
    }
    // Backticks show only while editing, so the text changes length on focus and blur.
    const focus = () => {
      const offset = caretOffset(node);
      node.replaceChildren();
      if (current.kind !== 'Rule') node.textContent = current.text;
      else renderInline(node, current.inline, true);
      placeCaret(node, offset);
    };
    const compositionStart = () => {
      composing = true;
    };
    const compositionEnd = () => {
      composing = false;
    };
    node.addEventListener('focus', focus);
    node.addEventListener('blur', render);
    node.addEventListener('compositionstart', compositionStart);
    node.addEventListener('compositionend', compositionEnd);
    render();
    return {
      update(value: OutlineLine) {
        current = value;
        render();
      },
      destroy() {
        node.removeEventListener('focus', focus);
        node.removeEventListener('blur', render);
        node.removeEventListener('compositionstart', compositionStart);
        node.removeEventListener('compositionend', compositionEnd);
      },
    };
  }

  const markClass: Record<LintKind, string> = {
    Hedge: 'hedge',
    Modal: 'modal',
    Vague: 'vague',
  };

  function renderInline(
    node: HTMLElement,
    parts: InlineText[],
    editing: boolean,
  ) {
    for (const part of parts) {
      if (part.code) node.append(code(part.text, editing));
      else if (part.mark) {
        const mark = document.createElement('mark');
        mark.className = markClass[part.mark];
        mark.textContent = part.text;
        node.append(mark);
      } else node.append(document.createTextNode(part.text));
    }
  }

  function code(text: string, editing: boolean) {
    // Editing shows the exact source, so caret offsets match the rule's text.
    if (editing) return document.createTextNode(`\`${text}\``);
    const element = document.createElement('code');
    const opening = document.createElement('span');
    opening.className = 'agents-tick';
    opening.textContent = '`';
    const closing = opening.cloneNode(true);
    element.append(opening, document.createTextNode(text), closing);
    return element;
  }

  function caretOffset(node: HTMLElement) {
    const selection = document.getSelection();
    if (!selection?.rangeCount) return 0;
    const range = selection.getRangeAt(0).cloneRange();
    if (!node.contains(range.endContainer)) return 0;
    range.selectNodeContents(node);
    range.setEnd(
      selection.getRangeAt(0).endContainer,
      selection.getRangeAt(0).endOffset,
    );
    return range.toString().length;
  }

  function keydown(event: KeyboardEvent) {
    if (event.isComposing || event.ctrlKey || event.metaKey) return;
    const node = event.currentTarget;
    if (!(node instanceof HTMLElement)) return;
    const identifier = line.identifier;
    let action: OutlineAction | null = null;
    if (event.key === 'Enter') {
      if (!event.shiftKey && line.kind === 'Rule') submit(identifier);
      changed = false;
      action = { Enter: { identifier } };
    } else if (event.key === 'Tab')
      action = { Indent: { identifier, nested: !event.shiftKey } };
    else if (event.key === 'Backspace' && !node.textContent)
      action = { DeleteEmpty: { identifier } };
    else if (['ArrowUp', 'ArrowDown'].includes(event.key)) {
      const down = event.key === 'ArrowDown';
      if (event.altKey) action = { Move: { identifier, down } };
      else if (
        caretOffset(node) === (down ? (node.textContent?.length ?? 0) : 0)
      )
        action = { Neighbor: { identifier, down } };
    } else if (event.key === 'Escape') node.blur();
    if (action) {
      event.preventDefault();
      event.stopPropagation();
      send(
        action,
        event.key === 'Tab' || event.altKey ? caretOffset(node) : undefined,
      );
    }
  }
</script>

{#if line.kind === 'Source'}
  {#if line.text}<pre class="agents-source">{line.text}</pre>{/if}
{:else}
  <div
    class="agents-row"
    class:nested={line.nested}
    class:heading={line.kind === 'Section'}
    class:title={line.kind === 'Title'}
  >
    {#if line.kind === 'Rule'}<span class="agents-dot"></span>{/if}
    <div
      id={`agents-line-${line.identifier}`}
      class="agents-text"
      contenteditable="plaintext-only"
      role="textbox"
      tabindex="0"
      aria-label={line.kind === 'Section'
        ? 'Section title'
        : line.kind === 'Title'
          ? 'Document title'
          : 'Rule'}
      data-placeholder={line.kind === 'Section'
        ? 'Section title'
        : 'Write a rule'}
      spellcheck="false"
      use:editable={line}
      onblur={leave}
      oninput={(event) => {
        changed = true;
        send({
          Edit: {
            identifier: line.identifier,
            text: event.currentTarget.textContent ?? '',
          },
        });
      }}
      onkeydown={keydown}
    ></div>
    {#if line.kind === 'Rule' && flagged && !pending}<span class="agents-state"
        ><button
          class="mini"
          title="Send to Haiku"
          onmousedown={(event) => event.preventDefault()}
          onclick={() => {
            changed = false;
            submit(line.identifier);
          }}>Tighten ↵</button
        ></span
      >{/if}
  </div>
  {#if pending || rewrite?.note || rewrite?.original != null}
    <div
      class="agents-rewrite"
      class:flagged={Boolean(rewrite?.note)}
      role="status"
    >
      {#if pending}<span>Haiku · rewriting</span>{/if}
      {#if rewrite?.note}<span>{rewrite.note}</span>{/if}
      {#if rewrite?.original != null}<span>was <s>{rewrite.original}</s></span>
        <button
          class="mini"
          onmousedown={(event) => event.preventDefault()}
          onclick={() => send({ Undo: { identifier: line.identifier } })}
          >Undo</button
        >
      {/if}
    </div>
  {/if}
  {#if line.continuation}<pre
      class="agents-continuation">{line.continuation}</pre>{/if}
{/if}
