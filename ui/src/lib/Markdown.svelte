<script lang="ts">
  import DOMPurify from 'dompurify';
  import { marked } from 'marked';

  let { text }: { text: string } = $props();
  // Agent output is untrusted. Keep markdown formatting without executable HTML,
  // embedded content, remote images or attributes that can escape the transcript.
  // Every snapshot hands the transcript over as new objects, so this re-runs for every block
  // on every update. Parse only when the text itself changed: a long transcript with many
  // agents streaming would otherwise re-parse all of it many times a second (#273).
  let parsedText: string | undefined;
  let parsedHtml = '';
  const html = $derived.by(() => {
    if (text !== parsedText) {
      parsedHtml = renderMarkdown(text);
      parsedText = text;
    }
    return parsedHtml;
  });

  function renderMarkdown(source: string): string {
    const content = DOMPurify.sanitize(
      marked.parse(source, { async: false, gfm: true }),
      {
        RETURN_DOM: true,
        ALLOWED_TAGS: [
          'p',
          'br',
          'strong',
          'em',
          'del',
          'code',
          'pre',
          'blockquote',
          'ul',
          'ol',
          'li',
          'h1',
          'h2',
          'h3',
          'h4',
          'h5',
          'h6',
          'hr',
          'a',
          'table',
          'thead',
          'tbody',
          'tr',
          'th',
          'td',
        ],
        ALLOWED_ATTR: ['href', 'title', 'start'],
        ALLOW_DATA_ATTR: false,
        ALLOW_ARIA_ATTR: false,
      },
    );
    // RETURN_DOM yields the sanitized body element; anything else means nothing to show.
    if (!(content instanceof HTMLElement)) {
      return '';
    }
    // A transcript link must never navigate the IDE's own window away.
    for (const link of content.querySelectorAll('a')) {
      link.setAttribute('target', '_blank');
      link.setAttribute('rel', 'noopener noreferrer');
    }
    return content.innerHTML;
  }
</script>

<div class="markdown">
  <!-- The only HTML sink receives the allowlisted DOMPurify result above. -->
  <!-- eslint-disable-next-line svelte/no-at-html-tags -->
  {@html html}
</div>
