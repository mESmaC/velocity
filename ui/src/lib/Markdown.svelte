<script lang="ts">
  import DOMPurify from 'dompurify';
  import { marked } from 'marked';

  let { text }: { text: string } = $props();
  // Agent output is untrusted. Keep markdown formatting without executable HTML,
  // embedded content, remote images or attributes that can escape the transcript.
  const html = $derived(renderMarkdown(text));

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
