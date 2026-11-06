<script lang="ts">
  import { tick } from 'svelte'
  import { openUrl } from '@tauri-apps/plugin-opener'

  let {
    page,
    onclose,
  }: {
    page: string | null
    onclose: () => void
  } = $props()

  let dialog: HTMLDialogElement
  let openButton = $state<HTMLButtonElement>()

  $effect(() => {
    if (!page) {
      if (dialog?.open) dialog.close()
      return
    }
    if (!dialog.open) dialog.showModal()
    tick().then(() => openButton?.focus())
  })

  function finish(openCurseforge: boolean) {
    const target = page
    if (dialog.open) dialog.close()
    onclose()
    if (openCurseforge && target) openUrl(target)
  }
</script>

<dialog
  bind:this={dialog}
  aria-labelledby="server-pack-tag-title"
  aria-describedby="server-pack-tag-body"
  oncancel={(e) => {
    e.preventDefault()
    finish(false)
  }}
  onclose={() => page && onclose()}
>
  {#if page}
    <form
      method="dialog"
      onsubmit={(e) => {
        e.preventDefault()
        finish(true)
      }}
    >
      <h2 id="server-pack-tag-title">Tag the server pack on CurseForge</h2>
      <p id="server-pack-tag-body">
        We uploaded your server pack as an additional file of the client pack. It's not currently possible to set <span class="ui-label">Additional File Info</span> to <span class="ui-label">Server Pack</span> automatically, so you'll need to set it on the file's CurseForge page yourself and save.
      </p>
      <div class="actions">
        <button type="button" class="quiet" onclick={() => finish(false)}>Done</button>
        <button type="submit" class="primary" bind:this={openButton}>Open the server pack on CurseForge</button>
      </div>
    </form>
  {/if}
</dialog>

<style>
  dialog {
    width: min(460px, calc(100vw - 32px));
    max-height: calc(100vh - 64px);
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

  .ui-label {
    font-weight: 600;
  }

  .actions {
    display: flex;
    justify-content: end;
    gap: 6px;
  }
</style>
