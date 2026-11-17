<script lang="ts">
  import { convertFileSrc } from '@tauri-apps/api/core'
  import { open } from '@tauri-apps/plugin-dialog'
  import { api, errorText } from '../lib/api'

  let {
    images,
    onchange,
    quiet = false,
    label = 'Images attached to the announcement',
  }: {
    images: string[]
    onchange: (images: string[]) => void | Promise<void>
    // In a table row we drop the hint and use a smaller button.
    quiet?: boolean
    label?: string
  } = $props()

  const MAX_IMAGES = 4

  let busy = $state(false)
  let error = $state<string | null>(null)
  // The webview can't load an image until the backend has allowed its path.
  let viewable = $state<string[]>([])

  $effect(() => {
    const checked = [...images]
    if (!checked.length) {
      viewable = []
      return
    }
    let stale = false
    api.checkAnnouncementImages(checked).then(
      () => {
        if (stale) return
        viewable = checked
        error = null
      },
      (e) => {
        if (stale) return
        viewable = checked
        error = errorText(e)
      },
    )
    return () => (stale = true)
  })

  function fileName(path: string) {
    return path.split(/[\\/]/).pop() ?? path
  }

  async function attach() {
    const chosen = await open({ title: 'Choose images for the announcement', multiple: true, filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp'] }] })
    if (!chosen) return
    const added = [chosen].flat().filter((path) => !images.includes(path))
    if (!added.length) return
    const next = [...images, ...added]
    busy = true
    try {
      await api.checkAnnouncementImages(next)
      await onchange(next)
      error = null
    } catch (e) {
      error = errorText(e)
    } finally {
      busy = false
    }
  }

  async function remove(image: string) {
    busy = true
    try {
      await onchange(images.filter((path) => path !== image))
      error = null
    } catch (e) {
      error = errorText(e)
    } finally {
      busy = false
    }
  }
</script>

<div class="attached">
  {#if images.length}
    <ul aria-label={label}>
      {#each images as image, index (image)}
        <li>
          {#if viewable.includes(image)}
            <img src={convertFileSrc(image)} alt="" />
          {:else}
            <span class="blank" aria-hidden="true"></span>
          {/if}
          <span class="name">
            <span class="file" title={image}>{fileName(image)}</span>
          </span>
          <button type="button" class="ghost" disabled={busy} aria-label="Remove {fileName(image)}" onclick={() => remove(image)}>Remove</button>
        </li>
      {/each}
    </ul>
  {/if}
  <div class="add">
    <button type="button" class:ghost={quiet} class:busy disabled={busy || images.length >= MAX_IMAGES} onclick={attach}>{images.length ? 'Attach more images' : 'Attach images'}</button>
    {#if !quiet}<span class="muted small">Up to {MAX_IMAGES} images of 10 MB or less.</span>{/if}
  </div>
  {#if error}<p class="failed small" role="alert">{error}</p>{/if}
</div>

<style>
  .attached {
    display: grid;
    gap: 6px;
  }

  ul {
    display: grid;
    gap: 4px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  li {
    display: grid;
    grid-template-columns: 40px minmax(0, 1fr) auto;
    align-items: center;
    gap: 8px;
  }

  img,
  .blank {
    width: 40px;
    height: 40px;
    border: 1px solid var(--line-strong);
    border-radius: 4px;
    background: var(--surface);
    object-fit: cover;
  }

  .name {
    display: grid;
    min-width: 0;
  }

  .file {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .add {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 10px;
  }

  .ghost {
    color: var(--forest);
    white-space: nowrap;
  }

  .add .ghost {
    margin-left: -8px;
  }

  p {
    margin: 0;
  }
</style>
