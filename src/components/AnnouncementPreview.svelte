<script lang="ts">
  import { onDestroy, untrack } from 'svelte'
  import { changelogFitNote, errorText, type ComposedAnnouncement } from '../lib/api'
  import DiscordPreview from './DiscordPreview.svelte'

  let {
    compose,
    inputs,
    label,
    showFitNote = false,
  }: {
    compose: () => Promise<ComposedAnnouncement>
    // Whatever the composed post depends on. We compose again, after a short pause, when it changes.
    inputs: unknown
    label?: string
    // Publish already shows the note about a long changelog beside the preview, so only Releases asks for it here.
    showFitNote?: boolean
  } = $props()

  const RECOMPOSE_DELAY_MS = 400

  let composed = $state<ComposedAnnouncement | null>(null)
  let error = $state<string | null>(null)
  let composing = $state(false)
  let fitNote = $derived(showFitNote && composed ? changelogFitNote(composed) : null)
  let lastKey: string | null = null
  let cancelPending = () => {}

  // Stringifying reads every nested field, so a change deep inside the inputs counts too.
  $effect(() => {
    const key = JSON.stringify(inputs ?? null)
    untrack(() => schedule(key))
  })
  onDestroy(() => cancelPending())

  function schedule(key: string) {
    if (key === lastKey) return
    const delay = lastKey === null ? 0 : RECOMPOSE_DELAY_MS
    lastKey = key
    cancelPending()
    let stale = false
    composing = true
    const timer = setTimeout(() => {
      compose().then(
        (result) => {
          if (stale) return
          composed = result
          error = null
          composing = false
        },
        (e) => {
          if (stale) return
          error = `Couldn't put the post together: ${errorText(e)}`
          composing = false
        },
      )
    }, delay)
    cancelPending = () => {
      stale = true
      clearTimeout(timer)
    }
  }
</script>

<div class="announcement-preview" aria-busy={composing}>
  {#if fitNote}<p class="muted small">{fitNote}</p>{/if}
  {#if error}
    <p class="failed small" role="alert">{error}</p>
  {:else if !composed}
    <p class="muted small" role="status">Putting the post together.</p>
  {/if}
  {#if composed}
    <div class:stale={composing}><DiscordPreview {composed} {label} /></div>
  {/if}
</div>

<style>
  .announcement-preview {
    display: grid;
    gap: 6px;
  }

  p {
    margin: 0;
  }

  .stale {
    opacity: 0.7;
    transition: opacity 0.15s ease;
  }

  @media (prefers-reduced-motion: reduce) {
    .stale {
      transition: none;
    }
  }
</style>
