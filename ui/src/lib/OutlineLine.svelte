<script lang="ts">
  import type { OutlineAction, OutlineLine } from './ipc';

  let {
    line,
    send,
  }: {
    line: OutlineLine;
    send: (action: OutlineAction, caret?: number) => void;
  } = $props();

  // Keep the browser's live text node while typing; acknowledgements never replace the caret.
  function editable(node: HTMLElement, value: OutlineLine) {
    let current = value;
    function render() {
      if (document.activeElement === node) return;
      node.replaceChildren();
      if (current.kind !== 'Rule') {
        node.textContent = current.text;
        return;
      }
      renderInline(node, current);
    }
    const focus = () => {
      const selection = document.getSelection();
      const offset = caretOffset(node);
      node.textContent = current.text;
      if (node.firstChild && selection) {
        const range = document.createRange();
        range.setStart(node.firstChild, Math.min(offset, current.text.length));
        range.collapse(true);
        selection.removeAllRanges();
        selection.addRange(range);
      }
    };
    node.addEventListener('focus', focus);
    node.addEventListener('blur', render);
    render();
    return {
      update(value: OutlineLine) {
        current = value;
        render();
      },
      destroy() {
        node.removeEventListener('focus', focus);
        node.removeEventListener('blur', render);
      },
    };
  }

  function renderInline(node: HTMLElement, value: OutlineLine) {
    for (const part of value.inline) {
      if (!part.code) node.append(document.createTextNode(part.text));
      else {
        const code = document.createElement('code');
        const opening = document.createElement('span');
        opening.className = 'agents-tick';
        opening.textContent = '`';
        const closing = opening.cloneNode(true);
        code.append(opening, document.createTextNode(part.text), closing);
        node.append(code);
      }
    }
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
    if (event.key === 'Enter') action = { Enter: { identifier } };
    else if (event.key === 'Tab')
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
      oninput={(event) =>
        send({
          Edit: {
            identifier: line.identifier,
            text: event.currentTarget.textContent ?? '',
          },
        })}
      onkeydown={keydown}
    ></div>
  </div>
  {#if line.continuation}<pre
      class="agents-continuation">{line.continuation}</pre>{/if}
{/if}
