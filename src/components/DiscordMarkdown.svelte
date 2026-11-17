<script lang="ts">
  import { openUrl } from '@tauri-apps/plugin-opener'
  import { SvelteSet } from 'svelte/reactivity'
  import { emojiUrl, formatTimestamp, isEmojiOnly, isSafeUrl, parseBlocks, parseInline, type Block, type Inline, type MentionKind } from '../lib/discordMarkdown'

  let {
    text,
    inline = false,
    jumbo = false,
  }: {
    text: string
    // Embed titles take inline formatting only, and no links.
    inline?: boolean
    // Discord enlarges emoji in message content that has nothing else in it, but not in embeds.
    jumbo?: boolean
  } = $props()

  let blocks = $derived(inline ? [] : parseBlocks(text))
  let titleNodes = $derived(inline ? parseInline(text, { links: false }) : [])
  let bigEmoji = $derived(jumbo && isEmojiOnly(blocks))

  const failedEmoji = new SvelteSet<string>()
  const revealed = new SvelteSet<Inline>()

  const MENTION_LABELS: Record<MentionKind, string> = { user: '@user', role: '@role', channel: '#channel', everyone: '@everyone', here: '@here' }
  const MENTION_TITLES: Record<MentionKind, string> = { user: 'User', role: 'Role', channel: 'Channel', everyone: '', here: '' }

  function open(event: MouseEvent, url: string) {
    event.preventDefault()
    if (isSafeUrl(url)) openUrl(url)
  }

  function reveal(node: Inline, event: Event) {
    if (revealed.has(node)) return
    event.preventDefault()
    revealed.add(node)
  }
</script>

{#snippet inlines(nodes: Inline[])}{#each nodes as node, index (index)}{@render one(node)}{/each}{/snippet}

{#snippet one(node: Inline)}
  {#if node.type === 'text'}{node.text}{:else if node.type === 'br'}<br />{:else if node.type === 'bold'}<strong>{@render inlines(node.children)}</strong>{:else if node.type === 'italic'}<em>{@render inlines(node.children)}</em>{:else if node.type === 'underline'}<u>{@render inlines(node.children)}</u>{:else if node.type === 'strike'}<s>{@render inlines(node.children)}</s>{:else if node.type === 'code'}<code>{node.text}</code>{:else if node.type === 'link'}<a href={node.url} title={node.url} onclick={(e) => open(e, node.url)}>{@render inlines(node.children)}</a>{:else if node.type === 'emoji'}{#if failedEmoji.has(node.id)}:{node.name}:{:else}<img class="emoji" class:big={bigEmoji} src={emojiUrl(node)} alt=":{node.name}:" title=":{node.name}:" draggable="false" onerror={() => failedEmoji.add(node.id)} />{/if}{:else if node.type === 'mention'}<span class="mention" title={node.id ? `${MENTION_TITLES[node.kind]} ID ${node.id}` : undefined}>{MENTION_LABELS[node.kind]}</span>{:else if node.type === 'timestamp'}{@const date = new Date(node.unix * 1000)}<time class="timestamp" datetime={Number.isNaN(date.getTime()) ? undefined : date.toISOString()} title={formatTimestamp(node.unix, 'F')}>{formatTimestamp(node.unix, node.style)}</time>{:else if node.type === 'spoiler'}{#if revealed.has(node)}<span class="spoiler shown">{@render inlines(node.children)}</span>{:else}<span class="spoiler" role="button" tabindex="0" aria-label="Spoiler, reveal" onclick={(e) => reveal(node, e)} onkeydown={(e) => (e.key === 'Enter' || e.key === ' ') && reveal(node, e)}><span class="hidden" aria-hidden="true">{@render inlines(node.children)}</span></span>{/if}{/if}
{/snippet}

{#snippet blockList(items: Block[])}{#each items as block, index (index)}{@render blockOne(block)}{/each}{/snippet}

{#snippet blockOne(block: Block)}
  {#if block.type === 'paragraph'}
    <div class="line">{@render inlines(block.children)}</div>
  {:else if block.type === 'blank'}
    <div class="line" aria-hidden="true"><br /></div>
  {:else if block.type === 'heading'}
    <svelte:element this={`h${block.level}`} class="heading h{block.level}">{@render inlines(block.children)}</svelte:element>
  {:else if block.type === 'subtext'}
    <small class="subtext">{@render inlines(block.children)}</small>
  {:else if block.type === 'quote'}
    <blockquote>{@render blockList(block.children)}</blockquote>
  {:else if block.type === 'list'}
    {#if block.ordered}
      <ol start={block.start}>{#each block.items as item, index (index)}<li>{@render blockList(item)}</li>{/each}</ol>
    {:else}
      <ul>{#each block.items as item, index (index)}<li>{@render blockList(item)}</li>{/each}</ul>
    {/if}
  {:else if block.type === 'codeblock'}
    <pre><code>{block.text}</code></pre>
  {/if}
{/snippet}

{#if inline}{@render inlines(titleNodes)}{:else}<div class="markdown" class:big-emoji={bigEmoji}>{@render blockList(blocks)}</div>{/if}

<style>
  .markdown {
    overflow-wrap: anywhere;
  }

  .line,
  .heading,
  .subtext,
  pre code {
    white-space: pre-wrap;
  }

  .line {
    min-height: 1lh;
  }

  a {
    color: var(--discord-link);
    text-decoration: none;
  }

  a:hover {
    text-decoration: underline;
  }

  strong {
    font-weight: 700;
  }

  code {
    padding: 0.1em 0.25em;
    font-family: var(--font-log);
    font-size: 0.85em;
    background: var(--discord-code);
    border: 1px solid var(--discord-code-edge);
    border-radius: 4px;
    white-space: pre-wrap;
  }

  pre {
    margin: 6px 0 0;
    padding: 7px;
    max-width: 100%;
    overflow-x: auto;
    font-size: 0.875em;
    background: var(--discord-code);
    border: 1px solid var(--discord-code-edge);
    border-radius: 4px;
  }

  pre code {
    padding: 0;
    font-size: inherit;
    background: none;
    border: 0;
  }

  .heading {
    margin: 8px 0 0;
    font-weight: 700;
    line-height: 1.375;
    color: var(--discord-strong);
  }

  .heading:first-child {
    margin-top: 0;
  }

  .h1 {
    font-size: 1.5em;
  }

  .h2 {
    font-size: 1.25em;
  }

  .h3 {
    font-size: 1em;
  }

  .subtext {
    display: block;
    font-size: 0.8125em;
    color: var(--discord-muted);
  }

  blockquote {
    margin: 0;
    padding: 0 8px 0 12px;
    border-left: 4px solid var(--discord-quote);
    border-radius: 4px 0 0 4px;
  }

  ul,
  ol {
    margin: 4px 0 0 16px;
    padding: 0 0 0 4px;
  }

  li ul,
  li ol {
    margin-top: 0;
  }

  .emoji {
    width: 1.375em;
    height: 1.375em;
    margin: 0 0.05em;
    vertical-align: -0.4em;
    object-fit: contain;
  }

  .big-emoji {
    font-size: 2.75em;
    line-height: 1.2;
  }

  .emoji.big {
    width: 3rem;
    height: 3rem;
    vertical-align: -0.3em;
  }

  .mention {
    padding: 0 2px;
    font-weight: 500;
    color: var(--discord-mention-text);
    background: var(--discord-mention);
    border-radius: 3px;
  }

  .timestamp {
    padding: 0 2px;
    background: var(--discord-code);
    border-radius: 3px;
  }

  .spoiler {
    background: var(--discord-spoiler);
    border-radius: 3px;
  }

  .spoiler:not(.shown) {
    cursor: pointer;
  }

  .spoiler .hidden {
    opacity: 0;
  }

  .spoiler.shown {
    background: var(--discord-spoiler-shown);
  }

  .spoiler:focus-visible {
    outline: 2px solid var(--discord-link);
    outline-offset: 1px;
  }
</style>
