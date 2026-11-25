<script lang="ts">
  import { untrack } from 'svelte'
  import { openUrl } from '@tauri-apps/plugin-opener'
  import ContextMenu from './ContextMenu.svelte'
  import ModGallery from './ModGallery.svelte'
  import ModLinks from './ModLinks.svelte'
  import Notice from './Notice.svelte'
  import RemoteHtml from './RemoteHtml.svelte'
  import { api, errorText, type ModDetails, type ModPreview, type ModRequest, type Platform, type SearchResult } from '../lib/api'
  import { remoteTextHtml } from '../lib/markdown'
  import { fileIdCopyLabel, mavenCopyLabel, mavenSnippet, rawFileId, rawProjectId } from '../lib/maven'
  import type { MenuItem } from '../lib/menu'
  import { browse, cancelChange, changeFor, flashCopyNote, modUpdates, point, projectKey, queuedButton } from '../lib/modUpdates.svelte'
  import { BACK_PRIORITY, useBack } from '../lib/back.svelte'
  import { paneFocus } from '../lib/panes.svelte'
  import { presenceContext } from '../lib/presence.svelte'
  import { commandKey, isMac, shortcut } from '../lib/platform'

  let {
    platform,
    installedKeys,
    game,
    minecraft,
    loader,
    // The pack goes to both platforms, so results say when the other platform has the mod too.
    dual,
    install,
  }: {
    platform: Platform
    installedKeys: Set<string>
    game: string
    minecraft: string | null
    loader: string | null
    dual: boolean
    install: (requests: ModRequest[], projects: string[], names: string[]) => Promise<void>
  } = $props()

  // We wait for a pause in typing, so each keystroke doesn't spend a request.
  const SEARCH_DELAY_MS = 350
  const NAME = { curseforge: 'CurseForge', modrinth: 'Modrinth' } as const

  let results: SearchResult[] = $state([])
  let nextIndex: number | null = $state(null)
  let searching = $state(false)
  let searchError: string | null = $state(null)
  let resolving = $state(new Set<string>())
  // We ask before installing a Modrinth mod we couldn't find on CurseForge too, since the CurseForge
  // pack will need a decision about it.
  let confirming: { result: SearchResult; request: ModRequest; preview: ModPreview; onlyHere: boolean } | null = $state(null)
  // The Install button you just pressed turns into Cancel, so we hold back its red focus style
  // until focus leaves it and comes back.
  let pressed: string | null = $state(null)
  let previewError: { key: string; text: string } | null = $state(null)
  type Detail = 'description' | 'gallery'
  let reading: string | null = $state(null)
  let detail: Detail = $state('description')
  let descriptions: Record<string, string | { error: string }> = $state({})
  let detailsByKey: Record<string, ModDetails | { error: string }> = $state({})
  let loadingDetails = $state(new Set<string>())
  let ctrlHeld = $state(false)
  let copyError: string | null = $state(null)
  let menu: { x: number; y: number; label: string; items: MenuItem[] } | null = $state(null)
  // The other platform's page of each result it has too.
  let elsewhere: Record<string, string> = $state({})

  let resultsRegion: HTMLElement | undefined = $state()
  let name = $derived(NAME[platform])

  $effect(() => {
    presenceContext.browseModName = reading ? (results.find((r) => r.key === reading)?.name ?? null) : null
    return () => {
      presenceContext.browseModName = null
    }
  })

  let latestSearch = 0
  let timer: ReturnType<typeof setTimeout> | undefined

  async function run(append: boolean) {
    const id = ++latestSearch
    searching = true
    try {
      const page = await api.searchMods({ platform, text: browse.text, kind: browse.kind, category: browse.category, sort: browse.sort, index: append ? (nextIndex ?? 0) : 0 })
      if (id !== latestSearch) return
      // Popularity can shift between requests, so a project can turn up on two pages.
      const seen = new Set(results.map((r) => r.key))
      const fresh = append ? page.results.filter((r) => !seen.has(r.key)) : page.results
      results = append ? [...results, ...fresh] : fresh
      nextIndex = page.nextIndex
      searchError = null
      if (dual) lookForCopies(fresh)
    } catch (error) {
      if (id === latestSearch) searchError = `Couldn't search ${name}: ${errorText(error)}`
    } finally {
      if (id === latestSearch) searching = false
    }
  }

  // We only mark what we can confirm, so a failed lookup just leaves the marks off.
  async function lookForCopies(page: SearchResult[]) {
    const keys = page.map((r) => r.key)
    const found = await (platform === 'curseforge' ? api.modrinthCounterparts(keys) : api.curseforgeCounterparts(keys)).catch(() => [])
    for (const { key, url } of found) elsewhere[key] = url
  }

  let other = $derived(NAME[platform === 'curseforge' ? 'modrinth' : 'curseforge'])

  // Typing waits for a pause, and every other change to the search runs it right away.
  let lastText = browse.text
  $effect(() => {
    void [platform, browse.kind, browse.category, browse.sort]
    const text = browse.text
    clearTimeout(timer)
    if (text === lastText) untrack(() => run(false))
    else timer = setTimeout(() => run(false), SEARCH_DELAY_MS)
    lastText = text
    return () => clearTimeout(timer)
  })

  function loadDetails(key: string, force = false) {
    if ((!force && detailsByKey[key] !== undefined) || loadingDetails.has(key)) return
    if (force) delete detailsByKey[key]
    loadingDetails = new Set(loadingDetails).add(key)
    api.modDetails(key, [key]).then(
      (details) => {
        detailsByKey[key] = details
      },
      (error) => {
        detailsByKey[key] = { error: `Couldn't load details from ${name}: ${errorText(error)}` }
      },
    ).finally(() => {
      loadingDetails = new Set([...loadingDetails].filter((k) => k !== key))
    })
  }

  async function loadDescription(key: string, force = false) {
    if (!force && descriptions[key] !== undefined) return
    if (force) delete descriptions[key]
    try {
      descriptions[key] = remoteTextHtml(await api.modDescription(key)) || '<p>The author left this description empty.</p>'
    } catch (error) {
      descriptions[key] = { error: `Couldn't load the description from ${name}: ${errorText(error)}` }
    }
  }

  async function openDetail(key: string, show: Detail = detail) {
    if (reading === key && detail === show) {
      reading = null
      return
    }
    reading = key
    detail = show
    loadDetails(key)
    if (show === 'description' && descriptions[key] === undefined) {
      await loadDescription(key)
    }
  }

  function onRowClick(event: MouseEvent, result: SearchResult) {
    if ((event.target as HTMLElement).closest('button, a, input, select, .reading, .confirm')) return
    if (commandKey(event) && result.url) {
      event.preventDefault()
      openUrl(result.url)
      return
    }
    openDetail(result.key)
  }

  function onNameClick(event: MouseEvent, result: SearchResult) {
    if (commandKey(event) && result.url) {
      event.preventDefault()
      event.stopPropagation()
      openUrl(result.url)
      return
    }
    openDetail(result.key)
  }

  // Modrinth search doesn't return a version ID, so we resolve the newest fitting version on demand.
  async function resolveFile(result: SearchResult): Promise<string | null> {
    if (result.file) return result.file.file
    const files = await api.modVersions(`${browse.kind}/`, result.key)
    return files[0]?.file ?? null
  }

  async function copyText(text: string | null, label: string) {
    if (!text) return
    copyError = null
    try {
      await navigator.clipboard.writeText(text)
      flashCopyNote(`Copied ${label}.`)
    } catch (error) {
      copyError = `Couldn't copy ${label}: ${errorText(error)}`
    }
  }

  async function copySnippet(result: SearchResult) {
    copyError = null
    const label = platform === 'modrinth' ? 'Modrinth Maven snippet' : 'CurseMaven snippet'
    try {
      const file = await resolveFile(result)
      const snippet = mavenSnippet({ key: result.key, file, slug: result.slug, minecraft, loader })
      if (!snippet) {
        copyError = `Couldn't find a matching file for ${result.name} on ${game}.`
        return
      }
      await copyText(snippet, label)
    } catch (error) {
      copyError = `Couldn't copy ${label}: ${errorText(error)}`
    }
  }

  async function copyFileId(result: SearchResult) {
    copyError = null
    const label = platform === 'modrinth' ? 'version ID' : 'file ID'
    try {
      const fileId = rawFileId(await resolveFile(result))
      if (!fileId) {
        copyError = `Couldn't find a matching file for ${result.name} on ${game}.`
        return
      }
      await copyText(fileId, label)
    } catch (error) {
      copyError = `Couldn't copy ${label}: ${errorText(error)}`
    }
  }

  function openMenu(event: MouseEvent, result: SearchResult) {
    event.preventDefault()
    const row = event.currentTarget as HTMLElement
    const rect = row.getBoundingClientRect()
    const keyboard = !event.clientX && !event.clientY
    const hasCfFile = platform === 'curseforge' ? !!result.file : true
    const hasSnippet = hasCfFile && (platform === 'modrinth' || !!result.slug)
    const projectId = rawProjectId(result.key)
    menu = {
      x: keyboard ? rect.left + 24 : event.clientX,
      y: keyboard ? rect.bottom : event.clientY,
      label: `${result.name} actions`,
      items: [
        { label: 'Show description', run: () => openDetail(result.key, 'description') },
        { label: 'Show gallery', run: () => openDetail(result.key, 'gallery') },
        'separator',
        { label: `Open on ${name}`, disabled: !result.url, run: () => result.url && openUrl(result.url) },
        { label: 'Copy link', disabled: !result.url, run: () => copyText(result.url, 'link') },
        'separator',
        { label: mavenCopyLabel(platform), disabled: !hasSnippet, run: () => copySnippet(result) },
        { label: fileIdCopyLabel(platform), disabled: !hasCfFile, run: () => copyFileId(result) },
        ...(projectId ? [{ label: 'Copy project ID', run: () => copyText(projectId, 'project ID') }] : []),
      ],
    }
  }

  function onModifier(event: KeyboardEvent, down: boolean) {
    if (isMac ? event.key === 'Meta' : event.key === 'Control') ctrlHeld = down
  }

  // Modrinth's search doesn't say which version fits the pack, so for its results we name the
  // project and let packraven pick the newest version that does.
  function requestFor(result: SearchResult): ModRequest {
    return result.file ? { file: result.file.file, replaces: null } : { project: result.key, kind: browse.kind, replaces: null }
  }

  async function startInstall(result: SearchResult) {
    const request = requestFor(result)
    resolving = new Set(resolving).add(result.key)
    previewError = null
    try {
      const preview = await api.previewMods([request])
      const onlyHere = dual && platform === 'modrinth' && !elsewhere[result.key]
      if (preview.dependencies.length || preview.unavailable.length || onlyHere) confirming = { result, request, preview, onlyHere }
      else {
        await install([request], preview.projects, [result.name])
        recoverFocus()
      }
    } catch (error) {
      previewError = { key: result.key, text: errorText(error) }
    } finally {
      resolving = new Set([...resolving].filter((key) => key !== result.key))
    }
  }

  async function confirmInstall() {
    if (!confirming) return
    const { result, request, preview } = confirming
    confirming = null
    await install([request], preview.projects, [result.name, ...preview.dependencies])
    recoverFocus()
  }

  // Escape closes the prompt before it would leave the search, so the install you were weighing isn't lost.
  function onConfirmKey(event: KeyboardEvent, key: string) {
    if (event.key !== 'Escape') return
    event.preventDefault()
    cancelConfirm(key)
  }

  function cancelConfirm(key: string) {
    confirming = null
    document.getElementById(`install-${domId(key)}`)?.focus()
  }

  const inFocusedPane = paneFocus()

  useBack(BACK_PRIORITY.prompt, () => {
    if (!inFocusedPane() || !confirming) return false
    cancelConfirm(confirming.result.key)
    return true
  })

  useBack(BACK_PRIORITY.detail, () => {
    if (!inFocusedPane() || !reading) return false
    const readingEl = resultsRegion?.querySelector<HTMLElement>('.reading')
    const restoreFocus = !!(readingEl && document.activeElement && readingEl.contains(document.activeElement))
    const trigger = restoreFocus ? readingEl.closest('li')?.querySelector<HTMLElement>('button.name') : null
    reading = null
    if (trigger) trigger.focus({ preventScroll: true })
    return true
  })

  // Cancelling or confirming the dependency prompt removes the button you used, so focus falls to
  // the page. You may have moved on to another result by then, so we leave focus alone in that case.
  function recoverFocus() {
    if (document.activeElement === document.body) resultsRegion?.focus()
  }

  function list(names: string[]) {
    return names.length < 3 ? names.join(' and ') : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`
  }

  const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 })

  function date(iso: string) {
    return new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })
  }

  const typeLabels = { release: 'Release', beta: 'Beta', alpha: 'Alpha' }
  const domId = (key: string) => key.replace(/[^\w-]/g, '_')
</script>

<svelte:window onkeydown={(e) => onModifier(e, true)} onkeyup={(e) => onModifier(e, false)} onblur={() => (ctrlHeld = false)} />

<div class="browser">
  {#if searchError}
    <Notice tone="error">
      <p>{searchError}</p>
      <p><button onclick={() => run(false)} disabled={searching}>Try again</button></p>
    </Notice>
  {/if}
  {#if copyError}
    <Notice tone="error"><p>{copyError}</p></Notice>
  {/if}

  <p class="visually-hidden" role="status">
    {#if searching}Searching {name}.{:else if !searchError}{results.length} {results.length === 1 ? 'result' : 'results'}.{/if}
  </p>

  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="results" class:ctrl-open={ctrlHeld} role="region" aria-label="Results from {name}" aria-busy={searching} tabindex="0" bind:this={resultsRegion}>
    {#if !results.length && searching}
      <p class="muted empty">Searching {name}.</p>
    {:else if !results.length && !searchError}
      <p class="muted empty">
        {browse.text.trim()
          ? `${name} has nothing matching "${browse.text.trim()}" for ${game}.`
          : browse.category
            ? `${name} has nothing in this category for ${game}.`
            : `${name} has nothing of this type for ${game}.`}
      </p>
    {/if}
    <ul class:stale={searching && results.length}>
      {#each results as result (result.key)}
        {@const installed = installedKeys.has(result.key)}
        {@const key = projectKey(result.key)}
        {@const change = changeFor(key)}
        {@const shown = change && queuedButton(change, result.name)}
        {@const checking = resolving.has(result.key)}
        {@const unavailable = !!change || checking || confirming?.result.key === result.key}
        {@const otherUrl = elsewhere[result.key]}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_element_interactions -->
        <li class:has-url={!!result.url} onclick={(e) => onRowClick(e, result)} oncontextmenu={(e) => openMenu(e, result)}>
          {#if result.logo}<img src={result.logo} alt="" width="44" height="44" loading="lazy" />{:else}<span class="logo" aria-hidden="true"></span>{/if}
          <div class="about">
            <div class="title">
              <button
                class="name"
                aria-expanded={reading === result.key}
                aria-controls="about-{domId(result.key)}"
                title={result.url ? `${shortcut('Ctrl+Click')} to open on ${name}` : undefined}
                onclick={(e) => onNameClick(e, result)}
              >
                {result.name}{#if result.url}<svg class="ctrl-arrow" viewBox="0 0 12 12" aria-hidden="true"><path d="M4.5 2.5h5v5M9.5 2.5 3 9" /></svg>{/if}
              </button>
              {#if result.authors.length}<span class="muted">by {result.authors.join(', ')}</span>{/if}
            </div>
            <p class="summary">{result.summary}</p>
            <p class="meta muted">
              {compact.format(result.downloads)} downloads · Updated {date(result.updated)}{#if otherUrl}{' · '}<a href={otherUrl} onclick={(e) => { e.preventDefault(); openUrl(otherUrl) }}>Also on {other}<span class="visually-hidden">, opens in your browser</span></a>{/if}
            </p>
            {#if confirming?.result.key === result.key}
              <!-- svelte-ignore a11y_no_noninteractive_element_interactions (Escape here only closes the prompt; its buttons do the work) -->
              <div class="confirm" role="group" aria-label="Install {result.name}" onkeydown={(e) => onConfirmKey(e, result.key)} {@attach (node) => {
                node.scrollIntoView({ block: 'nearest' })
                node.querySelector<HTMLButtonElement>('button.primary')?.focus()
              }}>
                {#if confirming.onlyHere}
                  <p>
                    We didn't find {result.name} on CurseForge, so the CurseForge pack can't download it. Before you publish, you'll decide in Publish whether
                    the CurseForge pack carries it.
                  </p>
                {/if}
                {#if confirming.preview.dependencies.length}
                  <p>{result.name} requires {list(confirming.preview.dependencies)}, so we'll install {confirming.preview.dependencies.length === 1 ? 'it' : 'them'} too.</p>
                {/if}
                {#if confirming.preview.unavailable.length}
                  <p class="failed">It also requires {list(confirming.preview.unavailable)}, which has no file for {game}, so it may not load.</p>
                {/if}
                <p class="confirm-actions">
                  <button class="primary" onclick={confirmInstall}>
                    {confirming.preview.dependencies.length ? `Install all ${confirming.preview.dependencies.length + 1}` : confirming.preview.unavailable.length ? 'Install anyway' : `Install ${result.name}`}
                  </button>
                  <button class="quiet" onclick={() => cancelConfirm(result.key)}>Cancel</button>
                </p>
              </div>
            {/if}
            {#if previewError?.key === result.key}<p class="failed">Couldn't install {result.name}: {previewError.text}</p>{/if}
          </div>
          <div class="action">
            {#if installed}
              <span class="chip ok">Installed</span>
            {:else}
              {#if result.file && result.file.releaseType !== 'release' && !change}<span class="chip pending">{typeLabels[result.file.releaseType]}</span>{/if}
              <!-- One button carries every state, so it keeps keyboard focus while you work down the results. -->
              <button
                id="install-{domId(result.key)}"
                class:busy={shown?.busy || checking}
                class:cancels={shown?.cancels}
                class:pressed={pressed === result.key}
                class:pointed={shown?.cancels && modUpdates.pointing === change?.serial}
                aria-disabled={shown ? !shown.cancels : unavailable}
                onpointerenter={() => point(change, true)}
                onpointerleave={() => point(change, false)}
                onfocus={(e) => pressed !== result.key && e.currentTarget.matches(':focus-visible') && point(change, true)}
                onblur={() => {
                  point(change, false)
                  if (pressed === result.key) pressed = null
                }}
                onclick={() => {
                  if (change) return cancelChange(key, result.name)
                  if (unavailable) return
                  pressed = result.key
                  startInstall(result)
                }}
                aria-label={shown?.ariaLabel ?? `Install ${result.name}`}
                title={shown?.title}
              >
                {#if shown?.cancels}<span class="swap"><span>{shown.label}</span><span aria-hidden="true">Cancel</span></span>{:else}{shown?.label ?? (checking ? 'Checking' : 'Install')}{/if}
              </button>
            {/if}
          </div>
          {#if reading === result.key}
            {@const description = descriptions[result.key]}
            {@const entryDetails = detailsByKey[result.key]}
            {@const loadedDetails = entryDetails && !('error' in entryDetails) ? entryDetails : null}
            {@const detailsError = entryDetails && 'error' in entryDetails ? entryDetails.error : null}
            {@const galleryCount = loadedDetails?.gallery.length ?? 0}
            <div class="reading" id="about-{domId(result.key)}">
              <div class="detail-head">
                <div class="modes" role="group" aria-label="Show">
                  <button class="mode" aria-pressed={detail === 'description'} onclick={() => openDetail(result.key, 'description')}>Description</button>
                  <button class="mode" aria-pressed={detail === 'gallery'} onclick={() => openDetail(result.key, 'gallery')}>
                    Gallery{#if galleryCount}<span class="muted tab-count">{galleryCount}</span>{/if}
                  </button>
                </div>
                <ModLinks platformName={name} url={result.url} links={loadedDetails?.links ?? null} />
              </div>
              {#if detail === 'gallery'}
                <div class="gallery-wrap">
                  <ModGallery
                    name={result.name}
                    platformName={name}
                    items={loadedDetails?.gallery ?? []}
                    loading={entryDetails === undefined}
                    error={detailsError}
                    onretry={() => loadDetails(result.key, true)}
                  />
                </div>
              {:else if description === undefined}
                <p class="muted" role="status">Loading the description from {name}.</p>
              {:else if typeof description !== 'string'}
                <p class="failed">
                  {description.error}{' '}
                  <button class="link" onclick={() => loadDescription(result.key, true)}>Try again</button>
                </p>
              {:else}
                <!-- svelte-ignore a11y_no_noninteractive_tabindex (a long description needs to scroll by keyboard even when it has no links) -->
                <section class="description" aria-label="Description of {result.name}" tabindex="0">
                  <RemoteHtml html={description} topHeading={3} />
                </section>
              {/if}
            </div>
          {/if}
        </li>
      {/each}
    </ul>
    {#if nextIndex !== null}
      <p class="more">
        <button onclick={() => run(true)} disabled={searching} class:busy={searching}>Show more</button>
      </p>
    {/if}
  </div>
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} label={menu.label} items={menu.items} scroller={resultsRegion} onclose={() => (menu = null)} />
{/if}

<style>
  .browser {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-height: 0;
  }

  .results {
    container-type: inline-size;
    flex: 1;
    min-height: 240px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: var(--radius-panel);
  }

  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  li {
    display: grid;
    grid-template-columns: 44px 1fr auto;
    gap: 14px;
    align-items: start;
    padding: 14px 16px;
    cursor: pointer;
  }

  li:hover {
    background: var(--paper);
  }

  @media (max-height: 700px) {
    .browser {
      flex: none;
    }

    .results {
      flex: none;
      min-height: 0;
      overflow: visible;
    }
  }

  .stale {
    opacity: 0.55;
    transition: opacity 120ms ease-out;
  }

  /* At high zoom the results pane gets narrow, so the action drops under the text instead of
     squeezing it. */
  @container (max-width: 440px) {
    li {
      grid-template-columns: 32px 1fr;
      gap: 10px 12px;
    }

    li img,
    li .logo {
      width: 32px;
      height: 32px;
    }

    .action {
      grid-column: 2;
    }
  }

  li + li {
    border-top: 1px solid var(--line);
  }

  img,
  .logo {
    width: 44px;
    height: 44px;
    border-radius: var(--radius-small);
    object-fit: cover;
  }

  .logo {
    display: block;
    background: var(--line);
  }

  .about {
    display: grid;
    gap: 4px;
    min-width: 0;
  }

  .title {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 2px 8px;
  }

  .name {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 0;
    font: inherit;
    font-weight: 700;
    color: var(--text);
    text-align: left;
    background: transparent;
    border: 0;
  }

  .name:hover:not(:disabled) {
    background: transparent;
    text-decoration: underline;
    text-underline-offset: 0.18em;
  }

  .ctrl-arrow {
    display: none;
    width: 12px;
    height: 12px;
    flex: none;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.7;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .results.ctrl-open li.has-url:hover {
    cursor: alias;
  }

  .results.ctrl-open li.has-url:hover .name {
    color: var(--forest);
    text-decoration: underline;
    text-underline-offset: 0.18em;
  }

  .results.ctrl-open li.has-url:hover .ctrl-arrow {
    display: inline-block;
  }

  .reading {
    grid-column: 2 / -1;
    display: grid;
    justify-items: stretch;
    gap: 8px;
    max-width: 90ch;
    cursor: auto;
  }

  .detail-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px 16px;
  }

  .modes {
    width: fit-content;
  }

  .tab-count {
    margin-left: 6px;
    font-weight: 400;
    font-variant-numeric: tabular-nums;
  }

  .gallery-wrap {
    min-width: 0;
  }

  .reading p {
    margin: 0;
  }

  .description {
    justify-self: stretch;
    max-height: clamp(280px, 55vh, 720px);
    overflow: auto;
    padding: 14px 18px;
    border: 1px solid var(--line);
    border-radius: var(--radius-panel);
    background: var(--field);
  }

  .about p {
    margin: 0;
  }

  .summary {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    max-width: 72ch;
  }

  .meta {
    font-size: var(--step--1);
    font-variant-numeric: tabular-nums;
  }

  .meta a {
    color: inherit;
  }

  .action {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .confirm {
    display: grid;
    gap: 8px;
    margin-top: 6px;
    padding: 10px 12px;
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-control);
    background: var(--paper);
  }

  .confirm-actions {
    display: flex;
    gap: 8px;
  }

  .empty {
    padding: 14px 16px;
    margin: 0;
  }

  .more {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 12px 16px;
    margin: 0;
    border-top: 1px solid var(--line);
  }
</style>
