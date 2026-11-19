<script lang="ts">
  import { tick } from 'svelte'
  import { openUrl } from '@tauri-apps/plugin-opener'
  import type { GalleryItem } from '../lib/api'

  let {
    name,
    platformName,
    items,
    loading = false,
    error = null,
    onretry,
  }: {
    name: string
    platformName: string
    items: GalleryItem[]
    loading?: boolean
    error?: string | null
    onretry?: () => void
  } = $props()

  let viewing: number | null = $state(null)
  let imageLoading = $state(false)
  let stage: HTMLDivElement | undefined = $state()
  let closeButton: HTMLButtonElement | undefined = $state()
  let returnFocus: HTMLElement | null = null
  let current = $derived(viewing !== null ? (items[viewing] ?? null) : null)

  async function openLightbox(index: number) {
    returnFocus = document.activeElement as HTMLElement | null
    imageLoading = true
    viewing = index
    await tick()
    closeButton?.focus()
  }

  function closeLightbox() {
    viewing = null
    imageLoading = false
    const target = returnFocus
    returnFocus = null
    target?.focus()
  }

  function step(delta: number) {
    if (viewing === null || !items.length) return
    imageLoading = true
    viewing = (viewing + delta + items.length) % items.length
  }

  // Capture-phase listener stops Escape before Mods.svelte's window handler can close ModBrowser.
  $effect(() => {
    if (viewing === null) return
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault()
        event.stopPropagation()
        closeLightbox()
      } else if (event.key === 'ArrowLeft') {
        event.preventDefault()
        event.stopPropagation()
        step(-1)
      } else if (event.key === 'ArrowRight') {
        event.preventDefault()
        event.stopPropagation()
        step(1)
      } else if (event.key === 'Tab' && stage) {
        const focusable = Array.from(stage.querySelectorAll<HTMLElement>('button:not(:disabled), a[href], [tabindex]:not([tabindex="-1"])'))
        if (!focusable.length) return
        const first = focusable[0]
        const last = focusable[focusable.length - 1]
        if (event.shiftKey && document.activeElement === first) {
          event.preventDefault()
          last.focus()
        } else if (!event.shiftKey && document.activeElement === last) {
          event.preventDefault()
          first.focus()
        }
      }
    }
    window.addEventListener('keydown', onKey, true)
    return () => window.removeEventListener('keydown', onKey, true)
  })
</script>

{#if error}
  <p class="failed">
    {error}
    {#if onretry}{' '}<button class="link" onclick={onretry}>Try again</button>{/if}
  </p>
{:else if loading}
  <p class="muted" role="status">Loading gallery from {platformName}.</p>
{:else if !items.length}
  <p class="muted">This project has no gallery images on {platformName}.</p>
{:else}
  <section class="gallery" aria-label="Gallery of {name}">
    {#each items as item, index (`${item.url}:${index}`)}
      <button class="shot" onclick={() => openLightbox(index)} title={item.title ?? `Open image ${index + 1}`}>
        <img src={item.thumbnail} alt={item.title ?? `${name} screenshot ${index + 1}`} loading="lazy" />
        {#if item.title}
          <span class="caption">{item.title}</span>
        {/if}
      </button>
    {/each}
  </section>
{/if}

{#if current && viewing !== null}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
  <div class="lightbox" role="dialog" aria-modal="true" tabindex="-1" aria-label={current.title ?? `${name} image ${viewing + 1}`} onclick={(e) => e.target === e.currentTarget && closeLightbox()}>
    <div class="stage" bind:this={stage}>
      <div class="bar">
        <span class="counter muted">{viewing + 1} / {items.length}{#if current.title} · <strong class="label">{current.title}</strong>{/if}</span>
        <div class="bar-actions">
          <button class="link" onclick={() => openUrl(current.url)}>Open full image</button>
          <button class="ghost" bind:this={closeButton} onclick={closeLightbox} aria-label="Close">Close</button>
        </div>
      </div>
      <div class="frame" aria-busy={imageLoading}>
        {#if items.length > 1}
          <button class="nav prev" onclick={() => step(-1)} aria-label="Previous image">
            <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M10 3.5 5.5 8 10 12.5" /></svg>
          </button>
        {/if}
        {#key current.url}
          <img
            src={current.url}
            alt={current.title ?? `${name} screenshot ${viewing + 1}`}
            class:loading={imageLoading}
            onload={() => (imageLoading = false)}
            onerror={() => (imageLoading = false)}
          />
        {/key}
        {#if items.length > 1}
          <button class="nav next" onclick={() => step(1)} aria-label="Next image">
            <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M6 3.5 10.5 8 6 12.5" /></svg>
          </button>
        {/if}
      </div>
      {#if current.description}
        <p class="desc muted">{current.description}</p>
      {/if}
    </div>
  </div>
{/if}

<style>
  .gallery {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(min(180px, 100%), 1fr));
    gap: 10px;
    padding: 12px;
    border: 1px solid var(--line);
    border-radius: var(--radius-panel);
    background: var(--field);
    max-height: clamp(280px, 55vh, 640px);
    overflow: auto;
  }

  .shot {
    display: grid;
    grid-template-rows: auto 1fr;
    gap: 6px;
    padding: 4px;
    text-align: left;
    background: var(--paper);
    border: 1px solid var(--line);
    border-radius: var(--radius-control);
    overflow: hidden;
  }

  .shot:hover {
    border-color: var(--control-edge);
    background: var(--surface);
  }

  .shot img {
    width: 100%;
    aspect-ratio: 16 / 9;
    object-fit: cover;
    border-radius: var(--radius-small);
    display: block;
    background: var(--surface);
  }

  .caption {
    font-size: var(--step--2);
    color: var(--text);
    padding: 0 4px 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .lightbox {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: grid;
    place-items: center;
    padding: 24px;
    background: var(--backdrop);
  }

  .stage {
    display: grid;
    gap: 10px;
    max-width: min(1100px, 94vw);
    max-height: 90vh;
    overflow: auto;
    padding: 14px 16px;
    background: var(--paper);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-panel);
    box-shadow: 0 18px 48px var(--shadow);
  }

  .bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px 16px;
  }

  .counter {
    font-size: var(--step--1);
    font-variant-numeric: tabular-nums;
  }

  .label {
    color: var(--text);
    font-weight: 700;
  }

  .bar-actions {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .frame {
    position: relative;
    display: grid;
    place-items: center;
    min-height: 200px;
    background: var(--field);
    border: 1px solid var(--line);
    border-radius: var(--radius-control);
    overflow: hidden;
  }

  .frame img {
    max-width: min(1040px, 88vw);
    max-height: 70vh;
    object-fit: contain;
    display: block;
    transition: opacity 120ms ease-out;
  }

  .frame img.loading {
    opacity: 0.35;
  }

  .nav {
    position: absolute;
    top: 50%;
    transform: translateY(-50%);
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    padding: 0;
    border-radius: 999px;
    background: var(--paper);
    border: 1px solid var(--line-strong);
    opacity: 0.88;
  }

  .nav:hover {
    opacity: 1;
    background: var(--control);
  }

  .nav.prev {
    left: 10px;
  }

  .nav.next {
    right: 10px;
  }

  .nav svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.8;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .desc {
    margin: 0;
    font-size: var(--step--1);
    max-width: 80ch;
  }
</style>
