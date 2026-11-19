<script lang="ts">
  import { openUrl } from '@tauri-apps/plugin-opener'
  import type { ModLinks } from '../lib/api'

  let {
    platformName,
    url,
    links = null,
  }: {
    platformName: string
    url: string | null
    links?: ModLinks | null
  } = $props()

  const entries = $derived(
    [
      links?.discord ? { id: 'discord', label: 'Discord', url: links.discord } : null,
      links?.source ? { id: 'source', label: 'Source', url: links.source } : null,
      links?.wiki ? { id: 'wiki', label: 'Wiki', url: links.wiki } : null,
      links?.issues ? { id: 'issues', label: 'Issues', url: links.issues } : null,
    ].filter((entry): entry is { id: 'discord' | 'source' | 'wiki' | 'issues'; label: string; url: string } => entry !== null),
  )
</script>

{#if url || entries.length}
  <div class="ext-links" role="group" aria-label="Project links">
    {#each entries as entry (entry.id)}
      <button class="link ext-link" onclick={() => openUrl(entry.url)} title={entry.url}>
        {#if entry.id === 'discord'}
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M13 3.8A10.7 10.7 0 0 0 10.3 3l-.3.7a9.7 9.7 0 0 0-4 0L5.7 3A10.7 10.7 0 0 0 3 3.8C1.3 6.3.9 8.8 1.1 11.2a10.9 10.9 0 0 0 3.3 1.7l.7-1.1a7 7 0 0 1-1.1-.5l.3-.2a7.8 7.8 0 0 0 7.4 0l.3.2c-.4.2-.7.4-1.1.5l.7 1.1a10.9 10.9 0 0 0 3.3-1.7c.3-2.8-.5-5.2-1.9-7.4Z" />
            <circle cx="5.7" cy="8.4" r="1" fill="currentColor" stroke="none" />
            <circle cx="10.3" cy="8.4" r="1" fill="currentColor" stroke="none" />
          </svg>
        {:else if entry.id === 'source'}
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M5.5 4.5 2 8l3.5 3.5M10.5 4.5 14 8l-3.5 3.5M9 3 7 13" />
          </svg>
        {:else if entry.id === 'wiki'}
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M2.5 3.5h4a2 2 0 0 1 1.5.7 2 2 0 0 1 1.5-.7h4v9h-4a2 2 0 0 0-1.5.7 2 2 0 0 0-1.5-.7h-4v-9Z" />
            <path d="M8 4.2v9" />
          </svg>
        {:else}
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <circle cx="8" cy="8" r="5.5" />
            <path d="M8 5.2v3.3M8 10.8v.2" />
          </svg>
        {/if}
        <span>{entry.label}</span>
      </button>
    {/each}
    {#if url}
      <button class="link ext-link" onclick={() => openUrl(url)} title={url}>
        <span>Open on {platformName}</span>
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <path d="M6.5 3.5h6v6M12.5 3.5 6 10" />
          <path d="M11.5 10.5v2h-8v-8h2" />
        </svg>
      </button>
    {/if}
  </div>
{/if}

<style>
  .ext-links {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 14px;
  }

  .ext-link {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    min-height: 24px;
    font-size: var(--step--1);
  }

  .ext-link svg {
    width: 14px;
    height: 14px;
    flex: none;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.4;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
</style>
