<script lang="ts">
  import { tick } from 'svelte'
  import { api, errorText } from '../lib/api'
  import { BACK_PRIORITY, useBack } from '../lib/back.svelte'
  import { job } from '../lib/jobs.svelte'
  import { nav } from '../lib/nav.svelte'

  let { version, lastTag, lastVersion, onchange }: { version: string | null; lastTag: string | null; lastVersion: string | null; onchange: () => unknown } = $props()

  let editing = $state(false)
  let draft = $state('')
  let problem: string | null = $state(null)
  let error: string | null = $state(null)
  let announcement = $state('')
  let input = $state<HTMLInputElement>()
  let opener: HTMLElement | null = null
  let latestCheck = 0

  // We show the version you saved until the overview reads it back from the BCC config, with a
  // spinner so it's clear packraven hasn't confirmed it yet.
  let saving: string | null = $state(null)
  let shown = $derived(saving ?? version)
  let unreleased = $derived(!!shown && shown !== lastVersion)
  let released = $derived(lastTag ? `Last release ${lastTag}.` : 'No release yet.')
  let publishing = $derived(job('publish').phase === 'running')

  async function edit() {
    if (version === null) {
      nav.view = 'changes'
      return
    }
    if (publishing || saving !== null) return
    const active = document.activeElement
    opener = active instanceof HTMLElement && active !== document.body ? active : null
    draft = version
    problem = error = null
    editing = true
    await tick()
    input?.select()
  }

  // Changes and Publish ask for the editor through nav, since the rail lives outside the views.
  $effect(() => {
    if (nav.editVersion) {
      nav.editVersion = false
      edit()
    }
  })

  async function check() {
    const id = ++latestCheck
    const result = await api.versionProblem(draft).catch(() => null)
    if (id === latestCheck) problem = result
  }

  // When you opened the editor from the rail button, Svelte replaced that button, so the opener is
  // gone and we leave focus alone rather than highlighting the new one next to the chip.
  async function stop(refocus: boolean) {
    editing = false
    problem = null
    if (!refocus) return
    await tick()
    if (opener?.isConnected) opener.focus()
  }

  useBack(BACK_PRIORITY.prompt, () => {
    if (!editing) return false
    stop(true)
    return true
  })

  async function commit(refocus: boolean) {
    const next = draft.trim()
    if (next === version || (!next && !refocus)) return stop(refocus)
    const reason = await api.versionProblem(next).catch(() => null)
    if (reason) {
      if (refocus) {
        problem = reason
        return
      }
      await stop(false)
      error = `Didn't save ${next}. ${reason}`
      return
    }
    await stop(refocus)
    saving = next
    try {
      await api.setVersion(next)
      await onchange()
      error = null
      announcement = `Version set to ${next}.`
    } catch (e) {
      error = `Couldn't save the version: ${errorText(e)}`
    } finally {
      saving = null
    }
  }

  function onblur() {
    // Switching windows blurs the field too; the edit should still be there when you come back.
    if (editing && document.hasFocus()) commit(false)
  }

  // We move focus back to whatever opened the editor, often a button, and without preventDefault
  // the same Enter would press that button and reopen the editor.
  function onkeydown(event: KeyboardEvent) {
    if (event.key !== 'Enter' && event.key !== 'Escape') return
    event.preventDefault()
    if (event.key === 'Enter') commit(true)
    else stop(true)
  }
</script>

<div class="version-line" class:editing>
  {#if editing}
    <input
      bind:this={input}
      bind:value={draft}
      oninput={check}
      {onblur}
      {onkeydown}
      spellcheck="false"
      aria-label="Pack version"
      aria-invalid={!!problem}
      aria-describedby="version-note"
    />
  {:else}
    <button
      class="version"
      class:missing={shown === null}
      aria-disabled={publishing || saving !== null}
      aria-busy={saving !== null}
      title={shown === null
        ? 'Set up Better Compatibility Checker in Changes to give the pack a version.'
        : saving !== null
          ? 'Saving the version.'
          : publishing
            ? `packraven is publishing ${shown}.`
            : `Click to change the version. ${released}`}
      onclick={edit}
    >
      <span>{shown ?? 'No version'}</span>
      {#if saving !== null}
        <span class="saving" aria-hidden="true"></span>
      {:else if shown !== null && !publishing}
        <svg class="pencil" viewBox="0 0 16 16" aria-hidden="true"><path d="M10.5 3l2.5 2.5L6 12.5H3.5V10z M9 4.5l2.5 2.5" /></svg>
      {/if}
      <span class="visually-hidden">{shown === null ? ', set up in Changes' : saving !== null ? ', saving' : publishing ? ', publishing' : `, change the version. ${released}`}</span>
    </button>
    {#if unreleased}<span class="chip change">unreleased</span>{/if}
  {/if}
</div>
{#if editing}
  {#if problem}
    <p class="note problem" id="version-note">{problem}</p>
  {:else}
    <p class="note keys" id="version-note">
      <span><kbd>Enter</kbd> Save</span>
      <span class="divider" aria-hidden="true"></span>
      <span><kbd>Esc</kbd> Cancel</span>
    </p>
  {/if}
{:else if error}
  <p class="note problem" role="alert">{error}</p>
{/if}
<span class="visually-hidden" role="status">{announcement}</span>

<style>
  .version-line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
    min-width: 0;
  }

  .version,
  input {
    font-family: var(--font-log);
    font-variant-numeric: tabular-nums;
    font-size: var(--step-2);
    font-weight: 400;
    line-height: 1.1;
    color: var(--rail-strong);
  }

  /* The negative margin keeps the text where it sits in the rail while the hover area extends past it. */
  .version {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    margin: -2px -6px;
    padding: 2px 6px;
    text-align: left;
    overflow-wrap: anywhere;
    background: transparent;
    border-color: transparent;
  }

  .version:hover:not([aria-disabled='true']),
  .version:focus-visible {
    background: color-mix(in srgb, var(--rail-strong) 16%, transparent);
  }

  .version[aria-disabled='true'] {
    cursor: progress;
  }

  .version.missing {
    font-family: inherit;
    font-size: var(--step-0);
    color: var(--rail-muted);
  }

  .saving {
    flex: none;
    width: 12px;
    height: 12px;
    border: 2px solid var(--rail-muted);
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  .pencil {
    flex: none;
    width: 14px;
    height: 14px;
    fill: none;
    stroke: var(--rail-muted);
    stroke-width: 1.4;
    stroke-linejoin: round;
    opacity: 0;
  }

  .version:hover .pencil,
  .version:focus-visible .pencil {
    opacity: 1;
  }

  input {
    flex: 1 1 100%;
    width: 100%;
    min-width: 0;
    margin: -3px 0;
    padding: 2px 6px;
    background: var(--rail-pack);
    border-color: var(--rail-edge);
  }

  input[aria-invalid='true'] {
    border-color: var(--rail-fail);
  }

  .chip {
    font-size: var(--step--2);
  }

  .note {
    margin: 4px 0 0;
    font-size: var(--step--1);
    color: var(--rail-muted);
  }

  .note.problem {
    color: var(--rail-fail);
  }

  .keys {
    display: flex;
    align-items: center;
    gap: 5px;
    white-space: nowrap;
  }

  .keys kbd {
    padding: 0 4px;
    color: var(--rail-muted);
    border-color: var(--rail-edge);
  }

  .divider {
    width: 1px;
    height: 1em;
    background: var(--rail-edge);
  }
</style>
