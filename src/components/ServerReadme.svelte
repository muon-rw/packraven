<script lang="ts">
  import { onMount } from 'svelte'
  import { listen } from '@tauri-apps/api/event'
  import { api, errorText, type ServerReadme } from '../lib/api'

  let { onedit }: { onedit: (path: string) => void } = $props()

  const FILE = 'server-readme.txt'
  const placeholders: [string, string][] = [
    ['{name}', "the pack's name"],
    ['{version}', 'the version you build'],
    ['{minecraft}', 'the Minecraft version'],
    ['{loader}', 'NeoForge, Forge or Fabric'],
    ['{loader_version}', "the loader's version"],
    ['{setup}', 'what the first start does'],
    ['{packraven_folder}', 'a note on the packraven folder, when the server pack has one'],
  ]

  let readme = $state<ServerReadme | null>(null)
  let loadError = $state<string | null>(null)
  let editError = $state<string | null>(null)
  let busy = $state(false)

  async function load() {
    try {
      readme = await api.serverReadme()
      loadError = null
    } catch (error) {
      loadError = `Couldn't preview PLEASE READ - IMPORTANT!.txt: ${errorText(error)}`
    }
  }

  onMount(() => {
    load()
    // You edit or delete server-readme.txt in Files, often beside this view, so we follow the file.
    const unlisten = listen<{ paths: string[] }>('fs-change', ({ payload }) => {
      if (payload.paths.some((path) => path === FILE || path === '')) load()
    })
    return () => {
      unlisten.then((stop) => stop())
    }
  })

  async function edit() {
    editError = null
    busy = true
    try {
      await api.customizeServerReadme()
      await load()
      onedit(FILE)
    } catch (error) {
      editError = `Couldn't create ${FILE}: ${errorText(error)}`
    } finally {
      busy = false
    }
  }

  let unknown = $derived(readme?.unknownPlaceholders ?? [])
</script>

<figure class="readme" aria-labelledby="readme-caption">
  <figcaption id="readme-caption">
    <span class="file">PLEASE READ - IMPORTANT!.txt</span>
    <span class="source" role="status">{readme?.custom ? 'Yours' : "packraven's default"}</span>
    <button type="button" class:busy disabled={busy} aria-describedby="readme-hint" onclick={edit}>{readme?.custom ? 'Edit in Files' : 'Customize in Files'}</button>
  </figcaption>
  {#if readme}
    <pre>{readme.text}</pre>
  {:else if loadError}
    <p class="failed">{loadError}</p>
  {/if}
  {#if unknown.length}
    <p class="warn">
      {unknown.join(', ')}
      {unknown.length === 1 ? "isn't a placeholder" : "aren't placeholders"} we fill in, so hosts will see {unknown.length === 1 ? 'it' : 'them'} as written.
    </p>
  {/if}
  <p class="hint" id="readme-hint">
    {#if readme?.custom}
      From <span class="file">{FILE}</span> in the pack repo. Delete that file to go back to packraven's default.
    {:else}
      To add your own notes, customize it and we'll copy it into the pack repo as <span class="file">{FILE}</span>.
    {/if}
  </p>
  <ul class="placeholders" aria-label="Placeholders we fill in">
    {#each placeholders as [placeholder, meaning] (placeholder)}
      <li title="We fill this in with {meaning}"><code>{placeholder}</code><span class="visually-hidden">, {meaning}</span></li>
    {/each}
  </ul>
  {#if editError}<p class="failed" role="alert">{editError}</p>{/if}
</figure>

<style>
  figure {
    display: grid;
    gap: 6px;
    margin: 0;
  }

  figcaption {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 10px;
    color: var(--muted);
  }

  .source {
    font-size: var(--step--1);
  }

  figcaption button {
    margin-left: auto;
    padding: 3px 10px;
    font-size: var(--step--1);
  }

  pre {
    margin: 0;
    max-height: 18lh;
    padding: 10px 12px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-log);
    font-size: var(--step--1);
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--radius-control);
  }

  .hint,
  .failed,
  .warn {
    font-size: var(--step--1);
  }

  .hint {
    color: var(--muted);
  }

  .failed {
    color: var(--fail);
  }

  .warn {
    color: var(--warn);
    font-weight: 700;
  }

  .placeholders {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  code {
    padding: 1px 6px;
    font-family: var(--font-log);
    font-size: var(--step--1);
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--radius-small);
  }
</style>
