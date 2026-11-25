<script lang="ts" module>
  export type OverrideItem = { source: string; isDir: boolean }
</script>

<script lang="ts">
  import { tick } from 'svelte'
  import { SvelteSet } from 'svelte/reactivity'
  import { api, errorText, type ModPreview, type PackFolder, type PackKind, type PackLoader, type SearchResult } from '../lib/api'
  import { changeQueued, dropChange, projectKey, startChange } from '../lib/modUpdates.svelte'
  import { outerArchive, packTarget } from '../lib/viewers'
  import Notice from './Notice.svelte'

  let {
    request,
    pack,
    onclose,
    ondone,
  }: {
    request: OverrideItem[] | null
    pack: string | null
    onclose: () => void
    ondone: (paths: string[], isDir: boolean, note: string) => void
  } = $props()

  type Option = { path: string; name: string; where: string; enabled?: boolean }
  type Found = { state: 'searching' } | { state: 'found'; result: SearchResult } | { state: 'missing' } | { state: 'failed'; message: string }

  const NEW = '\u0000new'
  const STORE_KEY = 'packraven.packOverrides'
  // Minecraft lists the packs in this folder in Options, Resource Packs, so it's always a place
  // for a resource pack, even without a loader.
  const VANILLA_RESOURCE_PACKS = 'resourcepacks'
  const DESCRIPTIONS: Record<PackLoader['id'], Record<PackKind, string>> = {
    paxi: {
      data: 'Loads the packs in config/paxi/datapacks, in an order you can set. Made for Fabric, Forge and NeoForge.',
      resource: 'Turns on the packs in config/paxi/resourcepacks, in an order you can set. Made for Fabric, Forge and NeoForge.',
    },
    openloader: { data: 'Loads the packs in config/openloader/data.', resource: 'Turns on the packs in config/openloader/resources.' },
    globalpacks: { data: 'Loads the folders and packs listed in config/global_packs.toml.', resource: 'Turns on the folders and packs listed in config/global_packs.toml.' },
    kubejs: { data: 'Loads data overrides directly from kubejs/data.', resource: 'Loads resource overrides directly from kubejs/assets.' },
  }

  let dialog: HTMLDialogElement
  let loaders = $state<PackLoader[] | null>(null)
  let ownFolder = $state<PackFolder | null>(null)
  let vanilla = $state<PackFolder | null>(null)
  let mode = $state<'choose' | 'offer'>('choose')
  let selected = $state(NEW)
  let newName = $state('')
  let newFolder = $state('')
  let fallback = $state('')
  let found = $state<Partial<Record<PackLoader['id'], Found>>>({})
  let confirming = $state<{ loader: PackLoader; preview: ModPreview } | null>(null)
  let adding = $state<PackLoader['id'] | null>(null)
  let added = $state<string[] | null>(null)
  const excluded = new SvelteSet<string>()
  const existingPaths = new SvelteSet<string>()
  let busy = $state(false)
  let error = $state<string | null>(null)
  let opened = 0
  // While CurseForge answers, focus waits on Cancel; we move it to the first Add button that
  // becomes available unless you've moved it yourself.
  let focusPending = false

  let items = $derived(request ?? [])
  let single = $derived(items.length === 1 ? items[0] : null)
  let allTargets = $derived(
    items.flatMap((item) => {
      const target = packTarget(item.source)
      return target ? [{ ...item, ...target, jar: baseName(outerArchive(item.source)) }] : []
    }),
  )
  let targets = $derived(allTargets.filter((t) => !excluded.has(t.source)))
  let kind = $derived<PackKind | null>(allTargets[0]?.kind ?? null)
  let noun = $derived(kind === 'resource' ? 'resource pack' : 'data pack')
  let everyone = $derived(kind === 'resource' ? 'Turn it on for everyone' : 'Load it in every world')
  let jars = $derived([...new Set((targets.length ? targets : allTargets).map((t) => t.jar))])
  let jarSource = $derived(jars.length === 1 ? jars[0] : jars.length === 2 ? list(jars) : `${jars.length} mods`)
  let allDirs = $derived(targets.length > 0 && targets.every((t) => t.isDir))
  let what = $derived(describeItems(targets))
  let installed = $derived(loaders?.filter((l) => l.installed) ?? [])
  // Offer only dedicated pack loaders to install; KubeJS shows up automatically when already present.
  let offerLoaders = $derived(loaders?.filter((l) => l.id !== 'kubejs') ?? [])
  let folders = $derived(
    [
      ...installed.flatMap((l) => l.folders.map((f) => ({ folder: f, by: l.name }))),
      ...(ownFolder ? [{ folder: ownFolder, by: '' }] : []),
      ...(vanilla ? [{ folder: vanilla, by: '' }] : []),
    ].filter(({ folder }, index, all) => all.findIndex((other) => other.folder.path === folder.path) === index),
  )
  let options = $derived(
    folders.flatMap(({ folder, by }) =>
      folder.packs
        .filter((p) => !p.zipped)
        .map((p): Option => ({ path: p.path, name: p.name, enabled: p.enabled, where: [folder.path, by === p.name ? '' : by, enabledLabel(p.enabled)].filter(Boolean).join(' · ') })),
    ),
  )
  let zipped = $derived(folders.flatMap(({ folder }) => folder.packs.filter((p) => p.zipped).map((p) => p.name)))
  let creatable = $derived(folders.filter(({ folder }) => folder.canCreate).map(({ folder }) => folder.path))
  let nameProblem = $derived.by(() => {
    const name = newName.trim()
    if (!name) return 'Name the new pack.'
    if (/[\\/:*?"<>|]/.test(name)) return 'A pack name can’t contain \\ / : * ? " < > or |.'
    const clash = folders.find(({ folder }) => folder.path === newFolder)?.folder.packs.find((p) => p.name.toLowerCase() === name.toLowerCase())
    return clash ? `${newFolder} already has a pack named ${clash.name}. Pick it above instead.` : null
  })
  let canCopy = $derived(targets.length > 0 && (selected === NEW ? !nameProblem && !!newFolder : options.some((o) => o.path === selected)))
  let selectedName = $derived(options.find((o) => o.path === selected)?.name ?? '')
  let existingCount = $derived(selected === NEW ? 0 : existingIn(selected))
  let allAlreadyThere = $derived(targets.length > 0 && existingCount === targets.length)
  let turnOnNote = $derived.by(() => {
    if (selected === NEW) return newFolder === VANILLA_RESOURCE_PACKS ? 'Players have to turn a new pack on in Options, Resource Packs, unless you add it to options.txt.' : null
    const option = options.find((o) => o.path === selected)
    return option?.enabled === false ? `Players have to turn ${option.name} on in Options, Resource Packs, since options.txt doesn't list it.` : null
  })

  function enabledLabel(enabled: boolean | undefined) {
    return enabled === undefined ? '' : enabled ? 'on in options.txt' : 'off in options.txt'
  }

  $effect(() => {
    if (!request) return
    const seq = ++opened
    reset()
    if (!dialog.open) dialog.showModal()
    load(seq)
  })

  function reset() {
    loaders = null
    ownFolder = null
    vanilla = null
    mode = 'choose'
    selected = NEW
    found = {}
    confirming = null
    adding = null
    added = null
    excluded.clear()
    existingPaths.clear()
    busy = false
    error = null
    focusPending = false
    fallback = kind === 'resource' ? 'resourcepacks' : 'datapacks'
    newName = `${baseName(pack ?? '') || 'Pack'} ${kind === 'resource' ? 'resources' : 'data'}`
  }

  async function load(seq: number) {
    if (!kind) return
    const remembered = recall(kind)
    try {
      const [list, own, resources] = await Promise.all([
        api.packLoaders(kind),
        remembered.folder ? api.packFolder(remembered.folder).catch(() => null) : null,
        kind === 'resource' ? api.packFolder(VANILLA_RESOURCE_PACKS).catch(() => null) : null,
      ])
      if (seq !== opened) return
      loaders = list
      ownFolder = own
      vanilla = resources
    } catch (e) {
      if (seq === opened) error = errorText(e)
      return
    }
    if (!installed.length && !ownFolder && !vanilla) {
      showOffer()
      return
    }
    selected = remembered.pack && options.some((o) => o.path === remembered.pack) ? remembered.pack : creatable.length ? NEW : (options[0]?.path ?? NEW)
    newFolder = creatable[0] ?? ''
    await tick()
    focusChoice()
  }

  function focusChoice() {
    dialog.querySelector<HTMLElement>('input[type=radio]:checked, #override-name, button[type=submit]')?.focus()
  }

  async function showOffer() {
    mode = 'offer'
    confirming = null
    error = null
    await tick()
    dialog.querySelector<HTMLElement>('#override-cancel')?.focus()
    focusPending = true
    for (const loader of offerLoaders) {
      if (found[loader.id]?.state === 'found') continue
      found[loader.id] = { state: 'searching' }
      api
        .searchMods({ platform: 'curseforge', text: loader.name, kind: 'mods', category: null, sort: 'relevance', index: 0 })
        .then((page) => {
          const result = page.results.find((r) => r.file && loader.projectIds.some((id) => r.key === `cf:${id}`))
          found[loader.id] = result ? { state: 'found', result } : { state: 'missing' }
        })
        .catch((e) => (found[loader.id] = { state: 'failed', message: errorText(e) }))
        .finally(focusFirstAdd)
    }
  }

  // We wait for the loaders listed before one, so focus lands on the recommended one when it's there.
  async function focusFirstAdd() {
    if (!focusPending || mode !== 'offer') return
    const first = offerLoaders.find((l) => found[l.id]?.state !== 'missing' && found[l.id]?.state !== 'failed')
    if (!first || found[first.id]?.state !== 'found') {
      if (!first) focusPending = false
      return
    }
    focusPending = false
    await tick()
    if (document.activeElement?.id === 'override-cancel') document.getElementById(`override-add-${first.id}`)?.focus()
  }

  async function add(loader: PackLoader, confirmed = false) {
    const lookup = found[loader.id]
    if (lookup?.state !== 'found' || adding) return
    if (!lookup.result.file) return
    const requests = [{ file: lookup.result.file.file, replaces: null }]
    adding = loader.id
    error = null
    focusPending = false
    try {
      const preview = confirmed && confirming ? confirming.preview : await api.previewMods(requests)
      if (!confirmed && (preview.dependencies.length || preview.unavailable.length)) {
        confirming = { loader, preview }
        await tick()
        document.getElementById('override-confirm')?.focus()
        return
      }
      const names = [lookup.result.name, ...preview.dependencies]
      const serial = startChange(preview.projects.map(projectKey), names, 'install')
      try {
        changeQueued(serial, await api.changeMods(requests))
      } catch (e) {
        dropChange(serial)
        throw e
      }
      confirming = null
      added = [loader.name, ...preview.dependencies.map(shortName)]
      loaders = (loaders ?? []).map((l) => (l.id === loader.id ? { ...l, installed: true } : l))
      mode = 'choose'
      selected = NEW
      newFolder = loader.folders.find((f) => f.canCreate)?.path ?? creatable[0] ?? ''
      await tick()
      focusChoice()
    } catch (e) {
      error = `Couldn't add ${loader.name}: ${errorText(e)}`
    } finally {
      adding = null
    }
  }

  function showFallback(event: Event) {
    const details = event.currentTarget as HTMLDetailsElement
    if (!details.open) return
    focusPending = false
    document.getElementById('override-fallback')?.focus()
    details.querySelector('.fallback-body')?.scrollIntoView({ block: 'nearest' })
  }

  async function cancelConfirm() {
    const id = confirming?.loader.id
    confirming = null
    await tick()
    if (id) document.getElementById(`override-add-${id}`)?.focus()
  }

  async function useOwnFolder() {
    const path = fallback.trim().replace(/\\/g, '/').replace(/^\/+|\/+$/g, '')
    error = null
    let folder: PackFolder
    try {
      folder = await api.packFolder(path)
    } catch (e) {
      error = errorText(e)
      return
    }
    const single = folder.canCreate ? null : folder.packs[0]
    if (single?.zipped) {
      error = `${single.name} is a zipped pack. Extract it in the file tree to add files to it.`
      return
    }
    ownFolder = folder
    selected = single?.path ?? NEW
    if (!single) newFolder = folder.path
    remember(kind!, { folder: folder.path })
    mode = 'choose'
    await tick()
    focusChoice()
  }

  // Files and folders a pack already overrides stay as they are, so you never lose your edits.
  $effect(() => {
    const packs = options.map((o) => o.path)
    const inside = allTargets.map((t) => t.inside)
    if (!packs.length || !inside.length) {
      existingPaths.clear()
      return
    }
    const wanted = packs.flatMap((packPath) => inside.map((rel) => `${packPath}/${rel}`))
    const parents = [...new Set(wanted.map((p) => p.slice(0, p.lastIndexOf('/'))))]
    let cancelled = false
    Promise.all(parents.map((dir) => api.listDir(dir).catch(() => []))).then((lists) => {
      if (cancelled) return
      const present = new Set(lists.flat().map((e) => e.path))
      existingPaths.clear()
      for (const path of wanted) if (present.has(path)) existingPaths.add(path)
    })
    return () => {
      cancelled = true
    }
  })

  function existingIn(packPath: string) {
    return targets.filter((t) => existingPaths.has(`${packPath}/${t.inside}`)).length
  }

  function toggleTarget(source: string) {
    if (!excluded.delete(source)) excluded.add(source)
  }

  function ignoreEnter(e: KeyboardEvent) {
    if (e.key === 'Enter') e.preventDefault()
  }

  async function copy() {
    if (!targets.length || !kind || !canCopy || busy) return
    const create = selected === NEW
    const destination = create ? `${newFolder}/${newName.trim()}` : selected
    const packName = baseName(destination)
    busy = true
    error = null
    try {
      const copied = await api.copyIntoPack(
        targets.map((t) => t.source),
        destination,
        create,
      )
      remember(kind, { pack: destination })
      const whose = jars.length === 1 ? `${jars[0]}'s` : "the mods'"
      const one = targets.length === 1 ? targets[0] : null
      const copiedNote = one
        ? !copied.created.length
          ? `${packName} already overrides this ${one.isDir ? 'folder' : 'file'}, so we ${one.isDir ? 'showed' : 'opened'} its copy.`
          : `Copied into ${packName}. Minecraft uses ${one.isDir ? 'these files' : 'this copy'} instead of ${whose}${copied.existing ? `, along with ${plural(copied.existing, 'file')} the pack already had` : ''}.`
        : !copied.created.length
          ? `${packName} already overrides these ${what}, so we showed its copies.`
          : `Copied ${what} into ${packName}. Minecraft uses these files instead of ${whose}${copied.existing ? `, along with ${plural(copied.existing, 'file')} the pack already had` : ''}.`
      const note = turnOnNote ? `${copiedNote} ${turnOnNote}` : copiedNote
      // close() clears request, which empties targets synchronously before ondone runs.
      const firstIsDir = !!targets[0]?.isDir
      close()
      ondone(copied.paths, firstIsDir, note)
    } catch (e) {
      error = errorText(e)
    } finally {
      busy = false
    }
  }

  function close() {
    opened++
    focusPending = false
    if (dialog.open) dialog.close()
    onclose()
  }

  function baseName(path: string) {
    return path.replace(/[\\/]+$/, '').split(/[\\/]/).pop() ?? ''
  }

  // CurseForge titles often carry the loader and versions, like "YUNG's API (NeoForge) [1.20.4 - 1.21.1 ONLY]".
  function shortName(name: string) {
    return name.replace(/\s*[[(].*$/, '') || name
  }

  type Remembered = { pack?: string; folder?: string }

  function stored(): Record<string, Partial<Record<PackKind, Remembered>>> {
    try {
      return JSON.parse(localStorage.getItem(STORE_KEY) ?? '{}')
    } catch {
      return {}
    }
  }

  function recall(kind: PackKind): Remembered {
    return (pack && stored()[pack]?.[kind]) || {}
  }

  function remember(kind: PackKind, value: Remembered) {
    if (!pack) return
    const all = stored()
    all[pack] = { ...all[pack], [kind]: { ...all[pack]?.[kind], ...value } }
    try {
      localStorage.setItem(STORE_KEY, JSON.stringify(all))
    } catch {
      // Without storage, we ask again next time.
    }
  }

  function list(names: string[]) {
    return names.length < 3 ? names.join(' and ') : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`
  }

  function plural(count: number, noun: string) {
    return `${count} ${noun}${count === 1 ? '' : 's'}`
  }

  function describeItems(list: { isDir: boolean }[]) {
    const noun = list.every((i) => !i.isDir) ? 'file' : list.every((i) => i.isDir) ? 'folder' : 'item'
    return plural(list.length, noun)
  }

  let copyLabel = $derived(
    targets.length === 1
      ? targets[0].isDir
        ? allAlreadyThere
          ? 'Show the override'
          : 'Copy and show'
        : allAlreadyThere
          ? 'Open the override'
          : 'Copy and open'
      : allAlreadyThere
        ? 'Show the overrides'
        : 'Copy and show',
  )
</script>

<dialog bind:this={dialog} aria-labelledby="override-title" aria-describedby="override-body" oncancel={(e) => { e.preventDefault(); close() }}>
  {#if allTargets.length && kind}
    <form method="dialog" onsubmit={(e) => { e.preventDefault(); if (mode === 'choose') copy() }}>
      <h2 id="override-title">Override in a {noun}</h2>
      <p id="override-body">
        {#if !targets.length}
          Tick at least one item to override.
        {:else if single}
          {#if allAlreadyThere}
            {selectedName} already overrides <code>{targets[0].inside}{single.isDir ? '/' : ''}</code>, so we'll {single.isDir ? 'show' : 'open'} its copy.
          {:else}
            We'll copy <code>{targets[0].inside}{single.isDir ? '/' : ''}</code> into a {noun}, where it replaces the {single.isDir ? 'files' : 'copy'} in {targets[0].jar}. Then you can edit {single.isDir ? 'them' : 'it'}.
          {/if}
        {:else if allAlreadyThere}
          {selectedName} already overrides {targets.length === 1 ? `this ${targets[0].isDir ? 'folder' : 'file'}` : `these ${what}`}, so we'll {targets.length === 1 && !targets[0].isDir ? 'open its copy' : 'show its copies'}:
        {:else if existingCount}
          We'll copy {what} into a {noun}, where {targets.length === 1 ? 'it replaces' : 'they replace'} the {allDirs ? 'files' : targets.length === 1 ? 'copy' : 'copies'} in {jarSource}, and keep {selectedName}'s {existingCount === 1 ? 'copy of 1 item it already has' : `copies of ${existingCount} items it already has`}:
        {:else}
          We'll copy {what} into a {noun}, where {targets.length === 1 ? 'it replaces' : 'they replace'} the {allDirs ? 'files' : targets.length === 1 ? 'copy' : 'copies'} in {jarSource}. Then you can edit {targets.length === 1 && !targets[0].isDir ? 'it' : 'them'}:
        {/if}
      </p>
      {#if !single}
        <ul class="targets" aria-label="Items to override">
          {#each allTargets as item (item.source)}
            <li>
              <label class="target">
                <input type="checkbox" checked={!excluded.has(item.source)} onchange={() => toggleTarget(item.source)} onkeydown={ignoreEnter} />
                <code>{item.inside}{item.isDir ? '/' : ''}</code>
                {#if allTargets.some((t) => t.jar !== allTargets[0].jar)}<span class="muted small">{item.jar}</span>{/if}
              </label>
            </li>
          {/each}
        </ul>
      {/if}

      {#if error}<Notice tone="error"><p>{error}</p></Notice>{/if}

      {#if !loaders && !error}
        <p class="muted" role="status">Looking for packs…</p>
      {:else if mode === 'offer'}
        <section class="offer" aria-labelledby="offer-title" aria-describedby="offer-body">
          <h3 id="offer-title">{everyone}</h3>
          <p class="muted" id="offer-body">
            {kind === 'data'
              ? 'Minecraft only loads a data pack you add to a world. Add a mod that loads it in every world:'
              : 'Players have to turn on a resource pack themselves. Add a mod that turns it on for everyone:'}
          </p>
          <ul class="loaders">
            {#each offerLoaders as loader, index (loader.id)}
              {@const lookup = found[loader.id]}
              {@const reason = `override-reason-${loader.id}`}
              <li>
                <div class="loader">
                  <span class="loader-name">{loader.name}{#if index === 0}<span class="chip change">Recommended</span>{/if}</span>
                  <span class="muted small">{DESCRIPTIONS[loader.id][kind]}</span>
                  {#if lookup?.state === 'searching'}
                    <span class="muted small" id={reason}>Checking CurseForge…</span>
                  {:else if lookup?.state === 'missing'}
                    <span class="muted small" id={reason}>Not on CurseForge for this pack's Minecraft version and loader.</span>
                  {:else if lookup?.state === 'failed'}
                    <span class="fail small" id={reason}>Couldn't search CurseForge: {lookup.message}</span>
                  {/if}
                </div>
                {#if lookup?.state === 'found' || lookup?.state === 'searching'}
                  <button
                    type="button"
                    id="override-add-{loader.id}"
                    class:primary={index === 0}
                    class:busy={adding === loader.id || lookup.state === 'searching'}
                    aria-disabled={lookup.state !== 'found' || !!adding || !!confirming}
                    aria-describedby={lookup.state === 'searching' ? reason : undefined}
                    onclick={() => !confirming && add(loader)}
                  >Add {loader.name}</button>
                {/if}
              </li>
              {#if confirming?.loader.id === loader.id}
                <li class="confirm" role="group" aria-label="Add {loader.name} and what it requires">
                  {#if confirming.preview.dependencies.length}
                    <p>{loader.name} requires {list(confirming.preview.dependencies.map(shortName))}, so we'll add {confirming.preview.dependencies.length === 1 ? 'it' : 'them'} too.</p>
                  {/if}
                  {#if confirming.preview.unavailable.length}
                    <p class="fail">{loader.name} also requires {list(confirming.preview.unavailable.map(shortName))}, which isn't on CurseForge for this pack, so it may not load.</p>
                  {/if}
                  <span class="confirm-actions">
                    <button type="button" class="quiet" onclick={cancelConfirm}>Back</button>
                    <button type="button" class="primary" id="override-confirm" class:busy={adding === loader.id} aria-disabled={adding === loader.id} onclick={() => add(loader, true)}>
                      Add {confirming.preview.dependencies.length ? `${confirming.preview.dependencies.length + 1} mods` : loader.name}
                    </button>
                  </span>
                </li>
              {/if}
            {/each}
          </ul>
        </section>

        <details class="fallback" ontoggle={showFallback}>
          <summary>No thanks, just create a {noun} in a folder</summary>
          <div class="fallback-body">
            <label for="override-fallback" class="visually-hidden">Folder for the new {noun}</label>
            <span class="fallback-row">
              <input id="override-fallback" class="file" bind:value={fallback} spellcheck="false" autocomplete="off" aria-describedby="override-fallback-note" onkeydown={(e) => e.key === 'Enter' && (e.preventDefault(), useOwnFolder())} />
              <button type="button" disabled={!fallback.trim()} onclick={useOwnFolder}>Continue</button>
            </span>
            <span class="muted small" id="override-fallback-note">
              {#if kind === 'data'}
                Minecraft won't load it from there by itself. Copy it into a world's datapacks folder to use it.
              {:else if fallback.trim() === 'resourcepacks'}
                Players turn it on in Options, Resource Packs.
              {:else}
                Point a mod or launcher at this folder so Minecraft loads it.
              {/if}
            </span>
          </div>
        </details>

        <div class="actions">
          <button type="button" class="quiet" id="override-cancel" onclick={close}>Cancel</button>
        </div>
      {:else if loaders}
        {#if added}
          <Notice tone="ok"><p>We're adding {list(added)} to the pack. Your new {noun} loads the next time Minecraft starts.</p></Notice>
        {/if}
        <fieldset>
          <legend>{options.length ? 'Pack' : `New ${noun}`}</legend>
          {#if options.length}
            <div class="options">
              {#each options as option (option.path)}
                {@const count = existingIn(option.path)}
                <label class="option">
                  <input type="radio" name="override-pack" value={option.path} bind:group={selected} />
                  <span class="option-text">
                    <span class="option-name">{option.name}</span>
                    <span class="muted small file" title={option.where}>{option.where}</span>
                  </span>
                  {#if count}
                    <span class="chip change">
                      {targets.length === 1 ? (targets[0].isDir ? 'Has this folder' : 'Has this file') : count === targets.length ? `Has all ${targets.length}` : `Has ${count} of ${targets.length}`}
                    </span>
                  {/if}
                </label>
              {/each}
              {#if creatable.length}
                <label class="option">
                  <input type="radio" name="override-pack" value={NEW} bind:group={selected} />
                  <span class="option-text"><span class="option-name">New {noun}</span></span>
                </label>
              {/if}
            </div>
          {/if}
          {#if selected === NEW && creatable.length}
            <div class="new-pack">
              <label class="field">
                Name
                <input
                  id="override-name"
                  bind:value={newName}
                  spellcheck="false"
                  autocomplete="off"
                  aria-invalid={!!nameProblem}
                  aria-describedby="override-name-problem"
                />
                <span class="fail small" id="override-name-problem" aria-live="polite">{nameProblem ?? ''}</span>
              </label>
              {#if creatable.length > 1}
                <label class="field">
                  Folder
                  <select bind:value={newFolder}>
                    {#each creatable as folder (folder)}<option value={folder}>{folder}</option>{/each}
                  </select>
                </label>
              {:else}
                <p class="muted small where">In <span class="file">{creatable[0]}</span></p>
              {/if}
            </div>
          {/if}
          {#if turnOnNote}<p class="muted small" role="status">{turnOnNote}</p>{/if}
          {#if zipped.length}
            <p class="muted small">
              We left out {zipped.length > 3 ? plural(zipped.length, 'zipped pack') : list(zipped)}, since we can't add files to a zipped pack. Extract one in the file tree to use it.
            </p>
          {/if}
        </fieldset>

        <div class="actions">
          {#if !installed.length}
            <button type="button" class="link" onclick={showOffer}>{everyone} instead</button>
          {/if}
          <button type="button" class="quiet" onclick={close}>Cancel</button>
          <button type="submit" class="primary" class:busy aria-disabled={!canCopy || busy}>{copyLabel}</button>
        </div>
      {:else}
        <div class="actions">
          {#if error}<button type="button" onclick={() => load(opened)}>Try again</button>{/if}
          <button type="button" class="quiet" onclick={close}>Close</button>
        </div>
      {/if}
    </form>
  {/if}
</dialog>

<style>
  dialog {
    width: min(560px, calc(100vw - 32px));
    max-height: calc(100vh - 64px);
    padding: 0;
    color: var(--text);
    background: var(--paper);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-panel);
    box-shadow: 0 12px 32px var(--shadow);
    /* Keeps what we scroll to clear of the sticky footer. */
    scroll-padding-bottom: 72px;
  }

  dialog::backdrop {
    background: var(--shadow);
  }

  form {
    display: grid;
    gap: 14px;
    padding: 20px 20px 0;
  }

  h2 {
    font-size: var(--step-1);
  }

  h3 {
    font-size: var(--step-0);
  }

  p {
    margin: 0;
  }

  code {
    overflow-wrap: anywhere;
  }

  .targets {
    max-height: 128px;
    overflow: auto;
    margin: -6px 0 0;
    padding: 4px 10px;
    list-style: none;
    font-size: var(--step--1);
    background: var(--field);
    border: 1px solid var(--line);
    border-radius: var(--radius-control);
  }

  .target {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin: 0;
    padding-block: 3px;
    cursor: pointer;
  }

  .target input {
    flex: none;
    transform: translateY(1px);
  }

  .target code {
    flex: 1;
    min-width: 0;
  }

  .target > .small {
    flex: none;
  }

  .small {
    font-size: var(--step--1);
  }

  .fail {
    color: var(--fail);
  }

  .offer {
    display: grid;
    gap: 8px;
  }

  .loaders {
    display: grid;
    margin: 0;
    padding: 0;
    list-style: none;
    border: 1px solid var(--line);
    border-radius: var(--radius-control);
    background: var(--field);
  }

  .loaders li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 12px;
  }

  .loaders li + li {
    border-top: 1px solid var(--line);
  }

  .loaders li > button {
    flex: none;
    white-space: nowrap;
  }

  .loaders li.confirm {
    flex-wrap: wrap;
    border-top-style: dashed;
    background: var(--surface);
  }

  .confirm-actions {
    display: flex;
    gap: 6px;
    margin-left: auto;
  }

  .loader {
    display: grid;
    gap: 2px;
    min-width: 0;
  }

  .loader-name,
  .option-name {
    font-weight: 700;
  }

  .loader-name .chip {
    margin-left: 8px;
    vertical-align: 1px;
  }

  summary {
    width: fit-content;
    cursor: pointer;
  }

  .fallback-body {
    display: grid;
    gap: 4px;
    margin-top: 8px;
  }

  .fallback-row {
    display: flex;
    gap: 6px;
  }

  .fallback-row input {
    flex: 1;
    min-width: 0;
  }

  fieldset {
    display: grid;
    gap: 10px;
    min-width: 0;
    margin: 0;
    padding: 0;
    border: 0;
  }

  legend {
    padding: 0;
    margin-bottom: 6px;
    font-weight: 700;
  }

  .options {
    display: grid;
    max-height: 240px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: var(--radius-control);
    background: var(--field);
  }

  .option {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 0;
    padding: 8px 12px;
    cursor: pointer;
  }

  .option + .option {
    border-top: 1px solid var(--line);
  }

  .option:has(input:checked) {
    background: var(--here);
  }

  .option-text {
    display: grid;
    flex: 1;
    min-width: 0;
  }

  .option-text .file {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .new-pack {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 6px 10px;
    align-items: start;
  }

  .field {
    display: grid;
    gap: 4px;
    margin: 0;
  }

  .field:only-child,
  .new-pack > .where {
    grid-column: 1 / -1;
  }

  .field:has(+ .where) {
    grid-column: 1 / -1;
  }

  .field .fail:empty {
    display: none;
  }

  input[aria-invalid='true'] {
    border-color: var(--fail-border);
  }

  /* The footer stays in view when the offer runs taller than a small window. */
  .actions {
    position: sticky;
    bottom: 0;
    display: flex;
    align-items: center;
    justify-content: end;
    gap: 6px;
    margin-top: -4px;
    padding: 4px 0 20px;
    background: var(--paper);
  }

  .actions .link {
    margin-right: auto;
  }
</style>
