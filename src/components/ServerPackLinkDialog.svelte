<script lang="ts">
  import { tick } from 'svelte'
  import { api, errorText } from '../lib/api'

  let {
    open,
    version,
    onclose,
    onrecorded,
  }: {
    open: boolean
    version: string
    onclose: () => void
    onrecorded: (fileId: number | null) => void
  } = $props()

  let dialog: HTMLDialogElement
  let input = $state<HTMLInputElement>()
  let link = $state('')
  let error = $state<string | null>(null)
  let saving = $state<'link' | 'skip' | null>(null)
  let busy = $derived(saving !== null)

  $effect(() => {
    if (!open) {
      if (dialog?.open) dialog.close()
      return
    }
    link = ''
    error = null
    saving = null
    if (!dialog.open) dialog.showModal()
    tick().then(() => input?.focus())
  })

  function close() {
    if (busy) return
    if (dialog.open) dialog.close()
    onclose()
  }

  async function save(value: string | null) {
    if (busy) return
    saving = value === null ? 'skip' : 'link'
    error = null
    try {
      const fileId = await api.markServerPackUploaded(version, value)
      if (dialog.open) dialog.close()
      onrecorded(fileId)
    } catch (e) {
      error = errorText(e)
      saving = null
      await tick()
      input?.focus()
      input?.select()
      return
    }
    saving = null
  }
</script>

<dialog
  bind:this={dialog}
  aria-labelledby="server-pack-link-title"
  aria-describedby="server-pack-link-body"
  oncancel={(e) => {
    e.preventDefault()
    close()
  }}
  onclose={() => open && close()}
>
  {#if open}
    <form
      method="dialog"
      onsubmit={(e) => {
        e.preventDefault()
        save(link)
      }}
    >
      <h2 id="server-pack-link-title">Track the {version} server pack</h2>
      <p id="server-pack-link-body">
        Paste the server pack's download link from CurseForge so packraven can follow its review. Right-click the down arrow next to its download count and copy the link.
      </p>
      <label class="field">
        <span class="label">Download link or file ID</span>
        <input
          type="text"
          bind:this={input}
          bind:value={link}
          oninput={() => (error = null)}
          placeholder="https://edge.forgecdn.net/files/..."
          autocomplete="off"
          spellcheck="false"
          readonly={busy}
          aria-busy={busy || undefined}
          aria-invalid={error ? 'true' : undefined}
          aria-describedby={error ? 'server-pack-link-error' : undefined}
        />
      </label>
      {#if error}<p id="server-pack-link-error" class="failed small" role="alert">{error}</p>{/if}
      <div class="actions">
        <button type="button" class="quiet" disabled={busy} onclick={close}>Cancel</button>
        <button type="submit" class="primary" class:busy={saving === 'link'} disabled={busy || !link.trim()}>{saving === 'link' ? 'Saving' : 'Track server pack'}</button>
        <button type="button" class="link skip" disabled={busy} onclick={() => save(null)}>
          {saving === 'skip' ? 'Saving' : 'Continue without a link'}
        </button>
      </div>
    </form>
  {/if}
</dialog>

<style>
  dialog {
    width: min(480px, calc(100vw - 32px));
    max-height: calc(100vh - 64px);
    overflow: auto;
    padding: 0;
    color: var(--text);
    background: var(--paper);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-panel);
    box-shadow: 0 12px 32px var(--shadow);
  }

  dialog::backdrop {
    background: var(--shadow);
  }

  form {
    display: grid;
    gap: 14px;
    padding: 20px;
  }

  h2 {
    margin: 0;
    font-size: var(--step-1);
  }

  p {
    margin: 0;
    max-width: 52ch;
  }

  .field {
    display: grid;
    gap: 6px;
  }

  .label {
    font-size: var(--step--1);
    color: var(--muted);
  }

  input {
    width: 100%;
    font-family: var(--font-log);
    font-variant-numeric: tabular-nums;
    font-size: var(--step--1);
  }

  input[aria-invalid='true'] {
    border-color: var(--fail-border);
  }

  .small {
    font-size: var(--step--1);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: end;
    gap: 10px 6px;
  }

  .skip {
    order: -1;
    margin-right: auto;
    padding: 6px 0;
    font-size: var(--step--1);
  }
</style>
