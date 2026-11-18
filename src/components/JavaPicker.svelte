<script lang="ts">
  import type { Snippet } from 'svelte'
  import Notice from './Notice.svelte'
  import { open } from '@tauri-apps/plugin-dialog'
  import { errorText, type JavaInstall } from '../lib/api'
  import { cancelJavaDownload, downloadJava, findJava, isOurGraalvm, javaName, javas, samePath, scanJava, type DownloadProgress } from '../lib/java.svelte'

  let {
    id,
    label,
    major,
    value = $bindable(),
    defaultName,
    defaultPath,
    needed,
    hint,
    status,
    onchange,
    ondownload,
  }: {
    id: string
    label: string
    major: number
    value: string
    defaultName: string
    defaultPath: string | null
    needed: boolean
    hint?: string
    status?: Snippet
    onchange?: () => void
    ondownload: (java: JavaInstall) => void
  } = $props()

  const LTS = [17, 21, 25]

  const installs = $derived(javas.scan?.installs ?? [])

  // Forge before Minecraft 1.17 crashes on Java 9 and later, so packs below Java 17 only get their own version.
  const usable = (m: number) => m === major || (major >= 17 && m > major)

  const groups = $derived.by(() => {
    const majors = [...new Set(installs.map((java) => java.major))].filter(usable).sort((a, b) => (a === major ? -1 : b === major ? 1 : a - b))
    return majors.map((m) => ({ major: m, javas: installs.filter((java) => java.major === m) }))
  })

  const listed = $derived(value ? installs.find((java) => usable(java.major) && samePath(java.path, value)) : undefined)
  const custom = $derived(value && !listed ? value : null)
  const customJava = $derived(findJava(custom))
  const defaultJava = $derived(findJava(defaultPath))
  const chosen = $derived(value || defaultPath)

  const defaultText = $derived.by(() => {
    if (defaultPath) return defaultJava ? javaName(defaultJava) : defaultPath
    if (javas.scan) return `no Java ${major} found`
    return javas.error ? "couldn't look for Java" : 'looking for Java'
  })

  // When the exact version is missing, we suggest the closest newer long-term release you already have.
  const installedNewer = $derived.by(() => {
    const majors = [...new Set(installs.map((java) => java.major))].filter((m) => m > major && usable(m))
    const best = majors.sort((a, b) => Number(LTS.includes(b)) - Number(LTS.includes(a)) || a - b)[0]
    if (best === undefined) return undefined
    return findJava(javas.scan?.automatic[best]) ?? installs.find((java) => java.major === best)
  })

  const graalvm = $derived(javas.scan?.downloads[major])
  const progress = $derived(graalvm ? javas.downloads[graalvm] : undefined)
  const missing = $derived(!chosen && javas.scan !== null)

  const chosenJava = $derived(findJava(chosen))
  const outdated = $derived(chosenJava && isOurGraalvm(chosenJava) && javas.outdated.includes(chosenJava.major) ? chosenJava : undefined)
  const updateProgress = $derived(outdated ? javas.downloads[outdated.major] : undefined)

  let downloadError = $state<string | null>(null)
  let downloadStatus = $state<string | null>(null)

  async function download() {
    downloadError = null
    downloadStatus = null
    try {
      const java = await downloadJava(major)
      if (!java) {
        downloadStatus = 'Download cancelled.'
        return
      }
      const name = `${java.distribution} ${java.version}`
      downloadStatus = java.major === major ? `Downloaded ${name}. packraven now uses it for Java ${major}.` : `Downloaded ${name} and chose it for Java ${major} under App settings.`
      ondownload(java)
    } catch (error) {
      downloadError = `Couldn't download GraalVM ${graalvm}: ${errorText(error)}`
    }
  }

  async function update(java: JavaInstall) {
    downloadError = null
    downloadStatus = null
    try {
      const updated = await downloadJava(java.major)
      if (!updated) {
        downloadStatus = 'Update cancelled.'
        return
      }
      downloadStatus = `Updated to ${updated.distribution} ${updated.version}.`
      ondownload(updated)
    } catch (error) {
      downloadError = `Couldn't update GraalVM ${java.major}: ${errorText(error)}`
    }
  }

  function selectJava(next: string) {
    if (next === value) return
    downloadStatus = null
    value = next
    onchange?.()
  }

  async function pick() {
    const file = await open({ title: `Choose a Java ${major} executable`, defaultPath: chosen || undefined })
    if (typeof file === 'string') selectJava(file)
  }

  const megabytes = (bytes: number) => Math.round(bytes / 1_000_000)
</script>

{#snippet downloading(graalvm: number, progress: DownloadProgress)}
  <div class="download">
    <progress max={progress.total ?? undefined} value={progress.total ? progress.received : undefined} aria-label="Downloading GraalVM {graalvm}"></progress>
    <button type="button" onclick={() => cancelJavaDownload(graalvm)}>Cancel</button>
  </div>
  <p class="muted note" role="status">
    Downloading GraalVM {graalvm}{progress.total ? `: ${megabytes(progress.received)} of ${megabytes(progress.total)} MB` : ''}
  </p>
{/snippet}

{#snippet offer()}
  {#if graalvm && progress}
    {@render downloading(graalvm, progress)}
  {:else}
    {#if installedNewer}
      <p class="note">
        You have {javaName(installedNewer)}, which runs most packs that need Java {major}, though some mods may break.
      </p>
    {/if}
    {#if installedNewer || graalvm}
      <div class="actions">
        {#if installedNewer}
          <button type="button" class:primary={needed} onclick={() => selectJava(installedNewer.path)}>Use {installedNewer.distribution} {installedNewer.version}</button>
        {/if}
        {#if graalvm}
          <button type="button" class:primary={needed && !installedNewer} onclick={download}>Download GraalVM {graalvm}</button>
        {/if}
      </div>
    {/if}
    {#if graalvm}
      <p class="muted note">
        Oracle's GraalVM, which your server pack also downloads on its first start{graalvm !== major ? `. GraalVM ${graalvm} runs packs that need Java ${major}` : ''}.
        It's over 300 MB, and packraven keeps it in its own folder.
      </p>
    {:else}
      <p class="note">
        packraven can't download Java {major} for you. Install one, such as Temurin from adoptium.net, then
        <button type="button" class="link" onclick={scanJava}>look again</button>.
      </p>
    {/if}
  {/if}
  {#if downloadError}<p class="failed" role="alert">{downloadError}</p>{/if}
{/snippet}

<div class="java">
  <div class="label-row">
    <label for={id}>{label}</label>
    {@render status?.()}
  </div>
  <div class="row">
    <select
      {id}
      value={listed?.path ?? value}
      onchange={(e) => selectJava(e.currentTarget.value)}
      aria-describedby={[chosen ? `${id}-path` : missing && `${id}-missing`, hint].filter(Boolean).join(' ') || undefined}
    >
      <option value="">{defaultName}: {defaultText}</option>
      {#each groups as group (group.major)}
        <optgroup label={group.major === major ? `Java ${major}` : `Java ${group.major} (newer, may break some mods)`}>
          {#each group.javas as java (java.path)}
            <option value={java.path}>{javaName(java)}</option>
          {/each}
        </optgroup>
      {/each}
      {#if custom}
        <optgroup label="Chosen file">
          <option value={custom}>{customJava ? `${javaName(customJava)}, not Java ${major}` : custom}</option>
        </optgroup>
      {/if}
    </select>
    <button type="button" onclick={pick}>Choose file</button>
  </div>
  {#if chosen}<p class="path file" id="{id}-path">{chosen}</p>{/if}
  {#if downloadStatus}<p class="muted note" role="status">{downloadStatus}</p>{/if}

  {#if outdated}
    <div class="quiet">
      {#if updateProgress}
        {@render downloading(outdated.major, updateProgress)}
      {:else}
        <p class="note">
          Oracle has published a newer build of GraalVM {outdated.major}, which can include security fixes. We'll move your Java settings to it and delete this
          one.
        </p>
        <div class="actions">
          <button type="button" class="primary" onclick={() => update(outdated)}>Update GraalVM {outdated.major}</button>
        </div>
      {/if}
      {#if downloadError}<p class="failed" role="alert">{downloadError}</p>{/if}
    </div>
  {/if}

  {#if missing}
    {#if needed}
      <Notice tone="warn">
        <p id="{id}-missing">A pack you added needs Java {major}, and packraven didn't find one on this computer. Its test runs can't start until you choose one.</p>
        {@render offer()}
      </Notice>
    {:else}
      <div class="quiet">
        <p class="muted note" id="{id}-missing">No Java {major} on this computer. None of your packs need it yet.</p>
        {@render offer()}
      </div>
    {/if}
  {/if}
</div>

<style>
  .java {
    display: grid;
    gap: 6px;
  }

  .label-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
  }

  .label-row > label {
    color: var(--text);
    font-size: var(--step-0);
  }

  .row {
    display: flex;
    gap: 8px;
  }

  /* A select is as wide as its longest option, and a percentage width lets it shrink with a narrow pane. */
  .row select {
    flex: 1;
    width: 100%;
    min-width: 0;
  }

  .path {
    color: var(--muted);
    overflow-wrap: anywhere;
  }

  .note {
    font-size: var(--step--1);
  }

  .quiet {
    display: grid;
    gap: 6px;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .download {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  progress {
    flex: 1;
    max-width: 320px;
    accent-color: var(--forest);
  }
</style>
