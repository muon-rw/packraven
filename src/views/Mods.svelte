<script lang="ts">
  import Notice from '../components/Notice.svelte'
  import JobLog from '../components/JobLog.svelte'
  import ModBrowser from '../components/ModBrowser.svelte'
  import ContextMenu from '../components/ContextMenu.svelte'
  import Tip from '../components/Tip.svelte'
  import RemoteHtml from '../components/RemoteHtml.svelte'
  import ClientLaunch from '../components/ClientLaunch.svelte'
  import DisableDialog, { type Dependent } from '../components/DisableDialog.svelte'
  import UpdateReview, { type PendingUpdate } from '../components/UpdateReview.svelte'
  import { onMount, tick, untrack } from 'svelte'
  import { openUrl } from '@tauri-apps/plugin-opener'
  import { listen } from '@tauri-apps/api/event'
  import { ask } from '@tauri-apps/plugin-dialog'
  import { api, errorText, type InstalledMod, type ModCheck, type ModFile, type Platform, type ReleaseType, type ModRequest } from '../lib/api'
  import { job, onJobEnd, serverPack } from '../lib/jobs.svelte'
  import { availableUpdate, browse, cancelChange, cancelMenuLabel, changeFor, changeQueued, downloadingCount, dropChange, isLocked, modUpdates, newestOnChannel, point, projectKey, queuedButton, startChange, waitingOnModrinth } from '../lib/modUpdates.svelte'
  import { remoteTextHtml } from '../lib/markdown'
  import { changedPart } from '../lib/fileNames'
  import type { MenuItem } from '../lib/menu'
  import { commandKey, shortcut, trashName } from '../lib/platform'
  import { paneFocus } from '../lib/panes.svelte'

  let {
    pack,
    game,
    publishesTo,
    clientRunning,
    ontest,
  }: {
    pack: string | null
    game: string
    // The platforms with a project in Pack settings. You can add mods from either platform before you set one.
    publishesTo: Platform[]
    clientRunning: boolean
    ontest: () => void
  } = $props()

  const PLATFORM_NAMES: Record<Platform, string> = { curseforge: 'CurseForge', modrinth: 'Modrinth' }
  const platformOf = (key: string | null): Platform => (key?.startsWith('mr:') ? 'modrinth' : 'curseforge')
  const domId = (text: string) => text.replace(/[^\w-]/g, '_')

  type Kind = 'mods' | 'resourcepacks' | 'shaderpacks' | 'datapacks'
  type SortKey = 'enabled' | 'name' | 'author' | 'update' | 'locked'
  type Detail = 'versions' | 'description'

  // We ask CurseForge and Modrinth again when you open the view after this long.
  const CHECK_MAX_AGE_MS = 15 * 60 * 1000

  let mods: InstalledMod[] = $state([])
  let loading = $state(true)
  let checking = $state(false)
  let loadError: string | null = $state(null)
  let checkError: string | null = $state(null)
  let actionError: string | null = $state(null)
  let kindFilter: Kind = $state('mods')
  let search = $state('')
  let sort: { key: SortKey; reversed: boolean } = $state({ key: 'update', reversed: false })
  let expanded: string | null = $state(null)
  let detail: Detail = $state('versions')
  let versions: ModFile[] | null = $state(null)
  let versionsError: string | null = $state(null)
  let descriptions: Record<string, string> = $state({})
  let descriptionError: string | null = $state(null)
  let removing: string | null = $state(null)
  let toggling: string | null = $state(null)
  let menu: { x: number; y: number; label: string; items: MenuItem[] } | null = $state(null)
  let disabling: { targets: InstalledMod[]; name: string; dependents: (Dependent & { mod: InstalledMod })[] } | null = $state(null)
  let reviewing: PendingUpdate[] | null = $state(null)
  let selected = $state(new Set<string>())
  let undo: { message: string; paths: string[] } | null = $state(null)
  let changelogs: Record<string, string | { error: string }> = $state({})
  let openChangelog: string | null = $state(null)

  let updating = $derived(job('mods').phase === 'running')
  let blocked = $derived(updating || clientRunning)
  let whyNoUpdate = $derived(clientRunning ? 'Close the test client first; it has these files open' : null)
  // The client never opens .disabled files, so you can enable them while it runs.
  let whyNoEnable = $derived(updating ? 'Wait for the downloads to finish' : null)
  let whyNoChange = $derived(whyNoUpdate ?? whyNoEnable)
  let showLog = $state(false)
  // The Update button you just pressed turns into Cancel, so we hold back its red focus style
  // until focus leaves it and comes back.
  let pressed: string | null = $state(null)

  // App remounts this view when you switch packs, so we only compare the pack once.
  untrack(() => {
    // A refusal only makes sense next to the run it happened in.
    if (job('mods').phase !== 'running') modUpdates.cancelNotes = []
    if (modUpdates.pack === pack) return
    modUpdates.pack = pack
    modUpdates.checks = null
    modUpdates.checkedAt = 0
    modUpdates.checkSerial++
  })

  async function load() {
    try {
      const installed = await api.installedMods()
      if (modUpdates.pack !== pack) return
      mods = installed.mods
      modUpdates.locked = new Set(installed.locked)
      loadError = null
    } catch (error) {
      if (modUpdates.pack === pack) loadError = `Couldn't read the mod list: ${errorText(error)}`
    } finally {
      if (modUpdates.pack === pack) loading = false
    }
  }

  async function check() {
    const serial = ++modUpdates.checkSerial
    checking = true
    try {
      const checks = await api.checkModUpdates()
      if (modUpdates.checkSerial !== serial || modUpdates.pack !== pack) return
      modUpdates.checks = new Map(checks.map((c) => [c.path, c]))
      modUpdates.checkedAt = Date.now()
      checkError = null
    } catch (error) {
      if (modUpdates.checkSerial === serial && modUpdates.pack === pack) checkError = `Couldn't check for updates: ${errorText(error)}`
    } finally {
      if (modUpdates.checkSerial === serial) checking = false
    }
  }

  onMount(async () => {
    api
      .getPackConfig()
      .then((config) => {
        if (modUpdates.pack === pack) modUpdates.channel = config.updates.channel
      })
      .catch(() => {})
    await load()
    const unchecked = mods.some((m) => !modUpdates.checks?.has(m.path))
    if (modUpdates.pack === pack && mods.length && (unchecked || Date.now() - modUpdates.checkedAt > CHECK_MAX_AGE_MS)) await check()

  })

  // The backend's file watcher updates mods.lock.json when something else changes the folder,
  // like the CurseForge app or a pull. Checks are keyed by path, so a list we didn't reload would
  // lose its update flags. A run reloads the list when it ends, so we leave its own changes to it.
  function checkIfStale() {
    if (checking || updating || document.hidden || modUpdates.pack !== pack || !mods.length) return
    const unchecked = mods.some((m) => !modUpdates.checks?.has(m.path))
    if (unchecked || Date.now() - modUpdates.checkedAt > CHECK_MAX_AGE_MS) check()
  }

  onMount(() => {
    window.addEventListener('focus', checkIfStale)
    document.addEventListener('visibilitychange', checkIfStale)
    const unlisten = listen('mod-list-changed', () => {
      if (!updating) reload()
    })
    return () => {
      unlisten.then((stop) => stop())
      window.removeEventListener('focus', checkIfStale)
      document.removeEventListener('visibilitychange', checkIfStale)
    }
  })

  async function reload() {
    await load()
    if (modUpdates.pack === pack && mods.some((m) => !modUpdates.checks?.has(m.path))) await check()
  }

  // You may have just uploaded a jar that CurseForge has since approved, so we ask about every jar it
  // didn't recognize before we reload the list.
  async function checkAgain() {
    checking = true
    await api.resyncModList(true).catch(() => {})
    if (modUpdates.pack !== pack) return
    await load()
    await check()
  }

  onJobEnd('mods', async (phase) => {
    finishedHere = true
    const before = mods.map((m) => m.path).join('\n')
    await load()
    if (expanded && !mods.some((m) => m.path === expanded)) expanded = null
    // One failed change doesn't undo the others in its run, so we compare the list itself. A run
    // you cancelled every change in finishes with no message.
    if ((phase === 'done' && job('mods').message) || mods.map((m) => m.path).join('\n') !== before) {
      serverPack.changedSinceBuild = true
      serverPack.changedSinceTest = true
    }
    await check()
  })

  async function setChannel(select: HTMLSelectElement) {
    const previous = modUpdates.channel
    modUpdates.channel = select.value as ReleaseType
    try {
      await api.setUpdateChannel(modUpdates.channel)
    } catch (error) {
      modUpdates.channel = previous
      select.value = previous
      actionError = `Couldn't save the update channel: ${errorText(error)}`
    }
  }

  async function setLocked(mod: InstalledMod, lock: boolean) {
    const key = mod.key
    if (key === null) return
    actionError = null
    try {
      await api.setModLocked(key, lock)
      const locked = new Set(modUpdates.locked)
      if (lock) locked.add(key)
      else locked.delete(key)
      modUpdates.locked = locked
    } catch (error) {
      actionError = `Couldn't ${lock ? 'lock' : 'unlock'} ${mod.name}: ${errorText(error)}`
    }
  }

  // Every enabled file that requires this one, directly or through another, nearest first, so
  // the dialog can nest each under the mod it needs.
  function dependentsOf(...roots: InstalledMod[]): (Dependent & { mod: InstalledMod })[] {
    const found: (Dependent & { mod: InstalledMod })[] = []
    const seen = new Set(roots.map((r) => r.key ?? r.path))
    const walk = (key: string | null, depth: number) => {
      if (!key) return
      for (const mod of mods) {
        if (seen.has(mod.key ?? mod.path) || mod.path.endsWith('.disabled')) continue
        if (!modUpdates.checks?.get(mod.path)?.requires.includes(key)) continue
        seen.add(mod.key ?? mod.path)
        found.push({ name: mod.name, path: mod.path, depth, mod })
        walk(mod.key, depth + 1)
      }
    }
    for (const root of roots) walk(root.key, 1)
    return found
  }

  async function setEnabled(targets: InstalledMod[], enabled: boolean) {
    targets = targets.filter((m) => m.path.endsWith('.disabled') === enabled)
    if (!targets.length) return
    if (!enabled) {
      // We only know what depends on a file from a check, so we finish one before asking.
      if (modUpdates.pack === pack && mods.some((m) => !modUpdates.checks?.has(m.path))) await check()
      const dependents = dependentsOf(...targets)
      if (dependents.length) {
        disabling = { targets, name: targets.length === 1 ? targets[0].name : `the ${targets.length} selected files`, dependents }
        return
      }
    }
    await setFilesEnabled(targets, enabled)
  }

  async function setFilesEnabled(targets: InstalledMod[], enabled: boolean) {
    // We leave the switches enabled while files rename, so they keep keyboard focus, and drop repeats.
    if (toggling) return
    toggling = targets[0].path
    actionError = null
    try {
      const paths = await api.setModsEnabled(targets.map((m) => m.path), enabled)
      const renamed = new Map(targets.map((m, i) => [m.path, paths[i]]))
      // Checks are keyed by path, so they follow each file to its new name.
      if (modUpdates.checks) modUpdates.checks = new Map([...modUpdates.checks].map(([key, value]) => [renamed.get(key) ?? key, { ...value, path: renamed.get(key) ?? key }]))
      if (expanded) expanded = renamed.get(expanded) ?? expanded
      mods = mods.map((m) => (renamed.has(m.path) ? { ...m, path: renamed.get(m.path)! } : m))
      selected = new Set([...selected].map((path) => renamed.get(path) ?? path))
      serverPack.changedSinceBuild = true
      serverPack.changedSinceTest = true
      // Turning several files off at once is easy to regret, so we offer to turn them back on.
      undo = !enabled && targets.length > 1 ? { message: `Disabled ${list(targets.map((m) => m.name))}.`, paths: paths } : null
    } catch (error) {
      const what = targets.length === 1 ? targets[0].name : `${targets.length} files`
      actionError = `Couldn't ${enabled ? 'enable' : 'disable'} ${what}: ${errorText(error)}`
    } finally {
      toggling = null
    }
  }

  function confirmDisable(cascade: boolean) {
    if (!disabling) return
    const { targets, dependents } = disabling
    disabling = null
    setFilesEnabled(cascade ? [...targets, ...dependents.map((d) => d.mod)] : targets, false)
  }

  function undoDisable() {
    if (!undo) return
    const paths = new Set(undo.paths)
    undo = null
    setFilesEnabled(mods.filter((m) => paths.has(m.path)), true)
  }

  async function setManyLocked(targets: InstalledMod[], lock: boolean) {
    for (const mod of targets.filter((m) => isLocked(m.key) !== lock)) await setLocked(mod, lock)
  }

  function pendingFor(targets: InstalledMod[]): PendingUpdate[] {
    return targets.filter((m) => updates.has(m.path) && !changeFor(m.path)).map((m) => ({ mod: m, file: updates.get(m.path)!, kind: isMod(m) ? null : kindLabels[folder(m.path)] }))
  }

  function confirmUpdates(chosen: PendingUpdate[]) {
    reviewing = null
    if (chosen.length) update(chosen.map((u) => ({ file: u.file.file, replaces: u.mod.path })))
  }

  async function showChangelog(key: string, file: string) {
    openChangelog = file
    if (changelogs[file] !== undefined) return
    try {
      changelogs[file] = remoteTextHtml(await api.modChangelog(key, file)) || '<p>The author left this changelog empty.</p>'
    } catch (error) {
      changelogs[file] = { error: `Couldn't load the changelog from ${PLATFORM_NAMES[platformOf(key)]}: ${errorText(error)}` }
    }
  }

  // Both platforms list versions newest first, so a version above the installed one is newer.
  // One older than every listed version isn't in the list at all.
  function isNewer(file: ModFile, installed: string | null, files: ModFile[]) {
    const at = files.findIndex((f) => f.file === installed)
    return at < 0 || files.indexOf(file) < at
  }

  function pickByArrow(event: KeyboardEvent, key: string) {
    if (!versions?.length || !['ArrowUp', 'ArrowDown', 'Home', 'End'].includes(event.key)) return
    event.preventDefault()
    const at = Math.max(0, versions.findIndex((f) => f.file === openChangelog))
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? versions.length - 1 : Math.min(versions.length - 1, Math.max(0, at + (event.key === 'ArrowDown' ? 1 : -1)))
    showChangelog(key, versions[next].file)
    const list = event.currentTarget as HTMLElement
    requestAnimationFrame(() => list.querySelectorAll<HTMLButtonElement>('.pick')[next]?.focus())
  }

  // The button you pressed is disabled or gone once a change starts, so we keep keyboard focus
  // on the list instead of letting it fall back to the page.
  function keepFocus(region: HTMLElement | undefined) {
    if (region && !region.contains(document.activeElement) && document.activeElement !== document.body) return
    region?.focus()
  }

  // Installs come from Browse, which names the mod you chose and the dependencies it brings.
  async function update(requests: ModRequest[], projects: string[] = [], installNames: string[] = []) {
    actionError = null
    finishedHere = false
    const replaced = requests.flatMap((r) => (r.replaces ? [r.replaces] : []))
    const names = [...replaced.map((path) => mods.find((m) => m.path === path)?.name ?? fileName(path)), ...installNames]
    const serial = startChange([...replaced, ...projects.map(projectKey)], names, replaced.length ? 'update' : 'install')
    try {
      changeQueued(serial, await api.changeMods(requests))
      // The review dialog closes once you confirm, which leaves focus nowhere.
      if (!browse.platform && document.activeElement === document.body) tableRegion?.focus()
    } catch (error) {
      dropChange(serial)
      actionError = `Couldn't start the update: ${errorText(error)}`
    }
  }

  function updateAll() {
    reviewing = pendingFor(mods)
  }

  async function remove(targets: InstalledMod[]) {
    // We only know what depends on a mod from a check, so we finish one before warning you.
    if (modUpdates.pack === pack && mods.some((m) => !modUpdates.checks?.has(m.path))) await check()
    const what = targets.length === 1 ? targets[0].name : `${targets.length} files`
    const dependents = dependentsOf(...targets).map((d) => d.name)
    const lines = [targets.length === 1 ? `Move ${fileName(targets[0].path)} to the ${trashName}?` : `Move these ${targets.length} files to the ${trashName}? ${list(targets.map((m) => m.name))}.`]
    if (dependents.length) lines.push(`${list(dependents)} ${dependents.length === 1 ? 'requires' : 'require'} ${targets.length === 1 ? what : 'them'}, directly or through another mod, so ${dependents.length === 1 ? 'it' : 'they'} may not load without ${targets.length === 1 ? 'it' : 'them'}.`)
    const confirmed = await ask(lines.join('\n\n'), { title: `Remove ${what}`, kind: 'warning', okLabel: `Move to ${trashName}`, cancelLabel: 'Cancel' })
    if (!confirmed) return
    const paths = targets.map((m) => m.path)
    removing = paths[0]
    actionError = null
    try {
      await api.removeMods(paths)
      serverPack.changedSinceBuild = true
      serverPack.changedSinceTest = true
      if (modUpdates.checks) modUpdates.checks = new Map([...modUpdates.checks].filter(([path]) => !paths.includes(path)))
      if (expanded && paths.includes(expanded)) expanded = null
      selected = new Set([...selected].filter((path) => !paths.includes(path)))
      await load()
      keepFocus(tableRegion)
    } catch (error) {
      actionError = `Couldn't remove ${what}: ${errorText(error)}`
    } finally {
      removing = null
    }
  }

  async function expand(mod: InstalledMod, show: Detail = detail) {
    if (expanded === mod.path && show === detail) {
      expanded = null
      return
    }
    const opening = expanded !== mod.path
    expanded = mod.path
    detail = show
    if (opening) {
      versions = null
      versionsError = null
      descriptionError = null
      openChangelog = null
      const key = mod.key
      if (!key) versions = []
      else
        api.modVersions(mod.path, key).then(
          (files) => {
            if (expanded !== mod.path) return
            versions = files
            // The newest file is the one you'd update to, so its changelog is the likeliest question.
            if (files.length) showChangelog(key, files[0].file)
          },
          (error) => expanded === mod.path && (versionsError = `Couldn't list the versions on ${PLATFORM_NAMES[platformOf(key)]}: ${errorText(error)}`),
        )
    }
    if (show === 'description' && mod.key && descriptions[mod.key] === undefined) {
      try {
        descriptions[mod.key] = remoteTextHtml(await api.modDescription(mod.key)) || '<p>The author left this description empty.</p>'
      } catch (error) {
        if (expanded === mod.path) descriptionError = `Couldn't load the description from ${PLATFORM_NAMES[platformOf(mod.key)]}: ${errorText(error)}`
      }
    }
  }

  // The whole row opens its details, except the controls that have a job of their own.
  function onRowClick(event: MouseEvent, mod: InstalledMod) {
    if ((event.target as HTMLElement).closest('button, a, input, select')) return
    expand(mod)
  }

  function openMenu(event: MouseEvent, mod: InstalledMod) {
    event.preventDefault()
    const row = event.currentTarget as HTMLElement
    const rect = row.getBoundingClientRect()
    const keyboard = !event.clientX && !event.clientY
    const lockedHere = isLocked(mod.key)
    const disabled = mod.path.endsWith('.disabled')
    const change = changeFor(mod.path)
    menu = {
      x: keyboard ? rect.left + 24 : event.clientX,
      y: keyboard ? rect.bottom : event.clientY,
      label: `${mod.name} actions`,
      items: [
        ...(change
          ? [change.stage === 'queued' ? { label: cancelMenuLabel(change), run: () => cancelChange(mod.path, mod.name) } : { label: queuedButton(change, mod.name).label, disabled: true, run: () => {} }, 'separator' as const]
          : updates.has(mod.path)
            ? [{ label: `Update to ${updates.get(mod.path)!.fileName}`, disabled: clientRunning, run: () => update([{ file: updates.get(mod.path)!.file, replaces: mod.path }]) }, 'separator' as const]
            : []),
        { label: 'Show versions', run: () => expand(mod, 'versions') },
        { label: 'Show description', run: () => expand(mod, 'description') },
        'separator',
        { label: `Open on ${PLATFORM_NAMES[platformOf(mod.key)]}`, disabled: !mod.url, run: () => mod.url && openUrl(mod.url) },
        { label: 'Copy link', disabled: !mod.url, run: () => mod.url && navigator.clipboard.writeText(mod.url) },
        'separator',
        // While a change runs, the version you'd lock to is about to change.
        { label: lockedHere ? 'Unlock' : 'Lock to this version', disabled: !!change, run: () => setLocked(mod, !lockedHere) },
        { label: disabled ? 'Enable' : 'Disable', disabled: disabled ? updating : blocked, run: () => setEnabled([mod], disabled) },
        'separator',
        { label: `Move to ${trashName}`, danger: true, disabled: blocked, run: () => remove([mod]) },
      ],
    }
  }

  function list(names: string[]) {
    return names.length < 3 ? names.join(' and ') : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`
  }

  // Every file of a mod shares most of its name, so we show only the part that sets it apart.
  function versionLabel(file: ModFile, files: ModFile[]) {
    const other = files.find((f) => f.fileName !== file.fileName)
    return other ? changedPart(other.fileName, file.fileName) : file.fileName
  }

  function fileName(path: string) {
    return path.slice(path.lastIndexOf('/') + 1)
  }

  function folder(path: string) {
    return path.slice(0, path.indexOf('/')) as Kind
  }

  function isMod(mod: InstalledMod) {
    return folder(mod.path) === 'mods'
  }

  function date(iso: string | null) {
    return iso ? new Date(iso).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) : ''
  }

  const typeLabels: Record<ReleaseType, string> = { release: 'Release', beta: 'Beta', alpha: 'Alpha' }
  const kindLabels: Record<Kind, string> = { mods: 'Mods', resourcepacks: 'Resource packs', shaderpacks: 'Shaders', datapacks: 'Data packs' }

  let updates = $derived(new Map(mods.map((m) => [m.path, availableUpdate(modUpdates.checks?.get(m.path))] as const).filter(([, file]) => file)) as Map<string, ModFile>)
  let updatable = $derived([...updates.keys()].filter((path) => !changeFor(path)).length)
  let duplicates = $derived.by(() => {
    const byProject = new Map<string, InstalledMod[]>()
    for (const mod of mods.filter((m) => isMod(m) && !m.path.endsWith('.disabled') && m.key)) byProject.set(mod.key!, [...(byProject.get(mod.key!) ?? []), mod])
    return [...byProject.values()].filter((group) => group.length > 1)
  })
  let duplicatePaths = $derived(new Set(duplicates.flat().map((m) => m.path)))
  let kindsWithUpdates = $derived(new Set([...updates.keys()].map(folder)))
  let kindCounts = $derived(mods.reduce((counts, m) => counts.set(folder(m.path), (counts.get(folder(m.path)) ?? 0) + 1), new Map<Kind, number>()))

  // Locked mods with a newer file sit between the ones you can update and the ones with nothing new.
  function updateTier(mod: InstalledMod) {
    if (updates.has(mod.path)) return 0
    return isLocked(mod.key) && newestOnChannel(checkFor(mod)) ? 1 : 2
  }

  // Each column sorts the interesting rows first: updates, locked or disabled mods, and A to Z.
  function compare(a: InstalledMod, b: InstalledMod): number {
    const byName = a.name.localeCompare(b.name)
    const flag = (value: (m: InstalledMod) => boolean) => Number(value(b)) - Number(value(a)) || byName
    switch (sort.key) {
      case 'update':
        return updateTier(a) - updateTier(b) || byName
      case 'locked':
        return flag((m) => isLocked(m.key))
      case 'enabled':
        return flag((m) => m.path.endsWith('.disabled'))
      case 'author':
        return (a.authors[0] ?? '￿').localeCompare(b.authors[0] ?? '￿') || byName
      default:
        return byName
    }
  }

  function sorted(a: InstalledMod, b: InstalledMod) {
    return sort.reversed ? -compare(a, b) : compare(a, b)
  }

  function matches(mod: InstalledMod) {
    const query = search.trim().toLowerCase()
    return !query || mod.name.toLowerCase().includes(query) || mod.path.toLowerCase().includes(query) || mod.authors.some((a) => a.toLowerCase().includes(query))
  }

  // While you search, each type tab counts its matches, so a match on another tab isn't missed.
  let matchCounts = $derived(mods.filter(matches).reduce((counts, m) => counts.set(folder(m.path), (counts.get(folder(m.path)) ?? 0) + 1), new Map<Kind, number>()))
  let visible = $derived(mods.filter((m) => folder(m.path) === kindFilter && matches(m)).sort(sorted))
  let selectedMods = $derived(mods.filter((m) => selected.has(m.path)))
  let allVisibleSelected = $derived(visible.length > 0 && visible.every((m) => selected.has(m.path)))

  function select(mod: InstalledMod, on: boolean) {
    const next = new Set(selected)
    if (on) next.add(mod.path)
    else next.delete(mod.path)
    selected = next
  }

  function selectVisible(on: boolean) {
    const next = new Set(selected)
    for (const mod of visible) {
      if (on) next.add(mod.path)
      else next.delete(mod.path)
    }
    selected = next
  }

  function sortBy(key: SortKey) {
    sort = { key, reversed: sort.key === key ? !sort.reversed : false }
  }

  function checkFor(mod: InstalledMod): ModCheck | undefined {
    return modUpdates.checks?.get(mod.path)
  }

  let summary = $derived.by(() => {
    if (checking) return 'Checking for updates.'
    if (!modUpdates.checks) return null
    if (updatable) return `${updatable} ${updatable === 1 ? 'has an update' : 'have updates'}.`
    return updates.size ? null : 'Everything is up to date.'
  })

  let runStatus = $derived.by(() => {
    const notes = modUpdates.cancelNotes
    const downloading = downloadingCount()
    if (updating) return [downloading ? `Downloading ${downloading} ${downloading === 1 ? 'file' : 'files'}.` : 'Changing mods.', ...notes].join(' ')
    return [finishedHere && job('mods').phase === 'done' ? job('mods').message : null, ...notes].filter(Boolean).join(' ')
  })

  let installedKeys = $derived(new Set(mods.flatMap((m) => m.keys)))

  let searchField: HTMLInputElement | undefined = $state()
  let tableRegion: HTMLElement | undefined = $state()
  let browseField: HTMLInputElement | undefined = $state()
  // A run that finished before you opened the view already shows in the list, so we only
  // report the ones that end while you're here.
  let finishedHere = $state(false)

  // Adding starts from the type you were looking at, and coming back puts you where you were.
  async function startAdding(platform: Platform) {
    browse.kind = kindFilter
    if (browse.platform !== platform) browse.category = null
    browse.platform = platform
    await tick()
    browseField?.focus()
  }

  // A pack that publishes elsewhere can't download mods only this platform has, so we gray out its
  // button and say why, without stopping you from browsing it.
  function elsewhereWarning(platform: Platform): string | null {
    if (!publishesTo.length || publishesTo.includes(platform)) return null
    const other = PLATFORM_NAMES[publishesTo[0]]
    return `This pack publishes to ${other} but not ${PLATFORM_NAMES[platform]}. The ${other} pack can't download mods only ${PLATFORM_NAMES[platform]} has, so you'd have to include them in its overrides.`
  }

  async function stopAdding() {
    const from = browse.platform
    browse.platform = null
    await tick()
    document.getElementById(`mods-add-${from}`)?.focus()
  }

  let categories: { id: string; name: string }[] = $state([])
  $effect(() => {
    const platform = browse.platform
    const kind = browse.kind
    if (!platform) return
    let current = true
    api.modCategories(platform, kind).then(
      (list) => current && (categories = list),
      () => current && (categories = []),
    )
    return () => (current = false)
  })

  // Another pack opens on its installed list.
  let browsedPack: string | null | undefined
  $effect(() => {
    if (browsedPack !== undefined && pack !== browsedPack) browse.platform = null
    browsedPack = pack
  })

  function setBrowseKind(kind: Kind) {
    browse.kind = kind
    browse.category = null
  }

  const inFocusedPane = paneFocus()

  function onKey(event: KeyboardEvent) {
    if (!inFocusedPane()) return
    if (event.key.toLowerCase() !== 'f' || event.shiftKey || !commandKey(event) || event.defaultPrevented) return
    event.preventDefault()
    const field = browse.platform ? browseField : searchField
    field?.focus()
    field?.select()
  }

  function onEscape(event: KeyboardEvent) {
    if (event.key !== 'Escape' || !browse.platform || !inFocusedPane() || event.defaultPrevented) return
    // Escape first clears a search you typed, like it does in the field.
    if (document.activeElement === browseField && browse.text) return
    event.preventDefault()
    stopAdding()
  }
</script>

<svelte:window onkeydown={(e) => { onKey(e); onEscape(e) }} />

<!-- Browsers won't break a line between a hyphen and a digit, so long jar names would hold the
     table wider than the view without these break points. -->
{#snippet fileText(name: string)}<span class="file">{#each name.split(/(?<=[-_+])/) as part, index (index)}{#if index}<wbr />{/if}{part}{/each}</span>{/snippet}

{#snippet sortHeader(key: SortKey, label: string, hidden = false)}
  <th aria-sort={sort.key === key ? (sort.reversed ? 'descending' : 'ascending') : undefined} class={key}>
    <button class="sort" class:sorted={sort.key === key} onclick={() => sortBy(key)}>
      {#if hidden}<span class="visually-hidden">{label}</span>{:else}{label}{/if}
      <svg viewBox="0 0 10 10" aria-hidden="true"><path d={sort.key === key && sort.reversed ? 'M2 6.5 5 3.5 8 6.5' : 'M2 3.5 5 6.5 8 3.5'} /></svg>
    </button>
  </th>
{/snippet}

<div class="mods">
  <header class="head">
    <div class="title">
      <h2>{browse.platform ? `Add from ${PLATFORM_NAMES[browse.platform]}` : 'Mods'}</h2>
      <p class="muted">
        {#if browse.platform}
          Mods, resource packs, shaders and data packs on {PLATFORM_NAMES[browse.platform]} that run on {game}.
          {#if elsewhereWarning(browse.platform)}<span class="elsewhere">{elsewhereWarning(browse.platform)}</span>{/if}
        {:else if loading && !mods.length}
          Reading the mod list.
        {:else if !mods.length}
          This pack has no mods yet.
        {:else}
          {mods.length} {mods.length === 1 ? 'file' : 'files'}. {summary ?? ''}
        {/if}
      </p>
      {#if !browse.platform && mods.length}
        <!-- The update channel is a pack setting, not a view filter, so it sits with the update summary. -->
        <label class="channel muted">
          Updates use
          <select value={modUpdates.channel} onchange={(e) => setChannel(e.currentTarget)}>
            <option value="alpha">any release type</option>
            <option value="beta">releases and betas</option>
            <option value="release">releases only</option>
          </select>
        </label>
      {/if}
    </div>
    <div class="actions">
      <!-- You often relaunch the client between mod changes, so we offer it here as well as in Run. -->
      <ClientLaunch running={clientRunning} />
      {#if !browse.platform}
        <div class="update-actions">
          <button onclick={checkAgain} disabled={checking || updating || !mods.length} class:busy={checking}>{checking ? 'Checking' : 'Check for updates'}</button>
          <Tip tip={updatable ? whyNoUpdate : null}>
            {#snippet children(why)}
              <button class="primary" class:busy={updating && !updatable} disabled={!updatable} aria-disabled={!!whyNoUpdate} aria-describedby={why} onclick={() => !whyNoUpdate && updateAll()}>
                {updatable ? `Update all (${updatable})` : updating ? 'Updating' : 'Update all'}
              </button>
            {/snippet}
          </Tip>
        </div>
      {/if}
    </div>
  </header>

  <!-- Adding and finding share one row in both modes, so the search field never moves. -->
  <div class="tab-row">
    <div class="add-from">
      {#if browse.platform}
        <button class="back" onclick={stopAdding} title="Back to the installed list (Esc)">
          <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M10 6H2M5 3 2 6l3 3" /></svg>
          Back to installed{#if mods.length}<span class="muted tab-count">{mods.length}<span class="visually-hidden"> files</span></span>{/if}
        </button>
      {:else}
        {#each ['curseforge', 'modrinth'] as const as platform (platform)}
          {@const warning = elsewhereWarning(platform)}
          <Tip tip={warning}>
            {#snippet children(why)}
              <button id="mods-add-{platform}" class:quiet={!!warning} aria-describedby={why} onclick={() => startAdding(platform)}>
                <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M6 2v8M2 6h8" /></svg>
                Add from {PLATFORM_NAMES[platform]}
              </button>
            {/snippet}
          </Tip>
        {/each}
      {/if}
    </div>
    <!-- A run reports here, in this row's spare room, rather than in a notice above the list, so
         queueing more changes doesn't move the rows you're clicking. -->
    <p class="run-status muted">
      <span role="status" title={runStatus}>{runStatus}</span>
      {#if updating}<button class="link" aria-expanded={showLog} aria-controls="mods-log" onclick={() => (showLog = !showLog)}>{showLog ? 'Hide log' : 'Show log'}</button>{/if}
    </p>
    {#if browse.platform}
      {@const where = PLATFORM_NAMES[browse.platform]}
      <input type="search" class="find" bind:this={browseField} bind:value={browse.text} placeholder="Search {where}" aria-label="Search {where}" title="Search {where} ({shortcut('Ctrl+F')})" />
    {:else}
      <input type="search" class="find" bind:this={searchField} bind:value={search} placeholder="Find a mod or author" aria-label="Find a mod or author" title="Find a mod or author ({shortcut('Ctrl+F')})" />
    {/if}
  </div>

  {#if loadError}
    <Notice tone="error">
      <p>{loadError}</p>
      <p><button onclick={() => { loadError = null; load() }}>Try again</button></p>
    </Notice>
  {/if}
  {#if checkError}
    <Notice tone="error">
      <p>{checkError}</p>
      <p><button onclick={check} disabled={checking}>Try again</button></p>
    </Notice>
  {/if}
  {#if clientRunning}
    <Notice tone="warn">
      <p>Close or kill the test client to update, disable or remove mods, since it has their files open. You can still add or enable mods, and the client will load them when you restart it.</p>
      <p><button onclick={ontest}>Show the client log</button></p>
    </Notice>
  {/if}
  <!-- While you add mods, notices about the installed files would push the results down. -->
  {#if duplicates.length && !browse.platform}
    <Notice tone="warn">
      <p>
        {duplicates.length === 1 ? `The mods folder has two versions of ${duplicates[0][0].name}` : `The mods folder has two versions of ${list(duplicates.map((group) => group[0].name))}`},
        and CurseForge will deny a pack uploaded with {duplicates.length === 1 ? 'it' : 'them'}. Remove the one you don't want. packraven won't publish until you do.
      </p>
      <p><button onclick={() => { browse.platform = null; kindFilter = 'mods'; search = duplicates[0][0].name }}>Show {duplicates.length === 1 ? 'them' : `${duplicates[0][0].name}`}</button></p>
    </Notice>
  {/if}
  {#if undo && !browse.platform}
    <Notice tone="ok">
      <p>
        {undo.message}
        <Tip tip={whyNoEnable}>
          {#snippet children(why)}<button class="link" onclick={() => !whyNoEnable && undoDisable()} aria-disabled={!!whyNoEnable} aria-describedby={why}>Undo</button>{/snippet}
        </Tip>
      </p>
    </Notice>
  {/if}
  {#if actionError}
    <Notice tone="error"><p>{actionError}</p></Notice>
  {/if}
  {#if updating && showLog}
    <div class="log" id="mods-log"><JobLog lines={job('mods').lines} label="Mod changes log" /></div>
  {:else if job('mods').phase === 'failed'}
    <Notice tone="error">
      <p>Couldn't finish changing mods: {job('mods').message}</p>
    </Notice>
    <details class="log" open>
      <summary>Log</summary>
      <JobLog lines={job('mods').lines} label="Mod changes log" />
    </details>
  {/if}

  <div class="panel" id="mods-panel">
    {#if browse.platform}
      <div class="tools">
        <div class="filters" role="group" aria-label="Type">
          {#each Object.keys(kindLabels) as Kind[] as kind (kind)}
            <button aria-pressed={browse.kind === kind} class:on={browse.kind === kind} onclick={() => setBrowseKind(kind)}>{kindLabels[kind]}</button>
          {/each}
        </div>
        <div class="refine">
          <label>
            <span class="visually-hidden">Category</span>
            <select bind:value={browse.category}>
              <option value={null}>All categories</option>
              {#each categories as category (category.id)}<option value={category.id}>{category.name}</option>{/each}
            </select>
          </label>
          <label class="sort">
            Sort by
            <select bind:value={browse.sort}>
              {#each [['relevance', 'Relevance'], ['popularity', 'Popularity'], ['downloads', 'Downloads'], ['newest', 'Newest'], ['updated', 'Recently updated']] as const as [sort, label] (sort)}
                <option value={sort}>{label}</option>
              {/each}
            </select>
          </label>
        </div>
      </div>
      {#key browse.platform}
        <ModBrowser platform={browse.platform} {installedKeys} {game} dual={publishesTo.length > 1} install={update} />
      {/key}
    {:else}
      <div class="tools">
        <div class="filters" role="group" aria-label="Show">
          {#each Object.keys(kindLabels) as Kind[] as kind (kind)}
            <button aria-pressed={kindFilter === kind} class:on={kindFilter === kind} onclick={() => (kindFilter = kind)}>
              {kindLabels[kind]}<span class="muted filter-count">{search.trim() ? (matchCounts.get(kind) ?? 0) : (kindCounts.get(kind) ?? 0)}</span>{#if search.trim()}<span class="visually-hidden"> matching</span>{/if}
              {#if kindsWithUpdates.has(kind)}<span class="has-updates" title="Has updates"><span class="visually-hidden">, has updates</span></span>{/if}
            </button>
          {/each}
        </div>
      </div>

      {#if selected.size}
        {@const pending = pendingFor(selectedMods)}
        <div class="bulk" role="toolbar" aria-label="Selected files">
          <span class="bulk-count">{selected.size} selected</span>
          <Tip tip={whyNoEnable}>
            {#snippet children(why)}<button onclick={() => !whyNoEnable && setEnabled(selectedMods, true)} disabled={!selectedMods.some((m) => m.path.endsWith('.disabled'))} aria-disabled={!!whyNoEnable} aria-describedby={why}>Enable</button>{/snippet}
          </Tip>
          <Tip tip={whyNoChange}>
            {#snippet children(why)}<button onclick={() => !whyNoChange && setEnabled(selectedMods, false)} disabled={selectedMods.every((m) => m.path.endsWith('.disabled'))} aria-disabled={!!whyNoChange} aria-describedby={why}>Disable</button>{/snippet}
          </Tip>
          <button onclick={() => setManyLocked(selectedMods, true)} disabled={selectedMods.every((m) => isLocked(m.key))}>Lock</button>
          <button onclick={() => setManyLocked(selectedMods, false)} disabled={!selectedMods.some((m) => isLocked(m.key))}>Unlock</button>
          <Tip tip={pending.length ? whyNoUpdate : null}>
            {#snippet children(why)}<button onclick={() => !whyNoUpdate && (reviewing = pending)} disabled={!pending.length} aria-disabled={!!whyNoUpdate} aria-describedby={why}>{pending.length ? `Update (${pending.length})` : 'Update'}</button>{/snippet}
          </Tip>
          <Tip tip={whyNoChange}>
            {#snippet children(why)}<button class="danger-quiet" onclick={() => !whyNoChange && remove(selectedMods)} aria-disabled={!!whyNoChange} aria-describedby={why}>Remove</button>{/snippet}
          </Tip>
          <button class="link clear" onclick={() => (selected = new Set())}>Clear selection</button>
        </div>
      {/if}

      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <div class="table" role="region" aria-label="Installed mods" tabindex="0" bind:this={tableRegion}>
        <table aria-busy={loading && !mods.length}>
          <thead>
            <tr>
              <th class="select">
                <input type="checkbox" checked={allVisibleSelected} indeterminate={!allVisibleSelected && visible.some((m) => selected.has(m.path))} onchange={(e) => selectVisible(e.currentTarget.checked)} aria-label="Select every {kindLabels[kindFilter].toLowerCase().replace(/s$/, '')} shown" disabled={!visible.length} />
              </th>
              {@render sortHeader('enabled', 'Enabled', true)}
              {@render sortHeader('name', 'Mod')}
              {@render sortHeader('author', 'Author')}
              {@render sortHeader('update', 'Update')}
              {@render sortHeader('locked', 'Locked', true)}
              <th class="trash"><span class="visually-hidden">Remove</span></th>
            </tr>
          </thead>
          <tbody>
            {#if loading && !mods.length}
              {#each { length: 8 } as _, row (row)}
                <tr class="skeleton" aria-hidden="true">
                  <td></td>
                  <td></td>
                  <td><span style="width: {55 + ((row * 37) % 35)}%"></span><span class="short"></span></td>
                  <td><span style="width: {40 + ((row * 23) % 30)}%"></span></td>
                  <td></td>
                  <td></td>
                  <td></td>
                </tr>
              {/each}
            {:else}
              <!-- Disabling a file renames it, so we key rows by what stays put and keep focus on the switch. -->
              {#each visible as mod (`${mod.key ?? mod.path}:${mod.file}`)}
                {@const status = checkFor(mod)}
                {@const next = updates.get(mod.path)}
                {@const lockedHere = isLocked(mod.key)}
                {@const held = newestOnChannel(status)}
                {@const disabled = mod.path.endsWith('.disabled')}
                {@const whyNoToggle = disabled ? whyNoEnable : whyNoChange}
                {@const change = changeFor(mod.path)}
                {@const logo = mod.logo ?? status?.logo}
                <tr class="row" class:open={expanded === mod.path} class:off={disabled} class:picked={selected.has(mod.path)} onclick={(e) => onRowClick(e, mod)} oncontextmenu={(e) => openMenu(e, mod)}>
                  <td class="select">
                    <input type="checkbox" checked={selected.has(mod.path)} onchange={(e) => select(mod, e.currentTarget.checked)} aria-label="Select {mod.name}" />
                  </td>
                  <td class="enabled">
                    <Tip tip={whyNoToggle}>
                      {#snippet children(why)}
                        <button
                          class="switch"
                          role="switch"
                          aria-checked={!disabled}
                          aria-label="Enable {mod.name}"
                          title={whyNoToggle ? undefined : disabled ? 'Disabled. We won\'t include it in exports.' : 'Enabled'}
                          aria-disabled={!!whyNoToggle}
                          aria-describedby={why}
                          aria-busy={toggling === mod.path}
                          onclick={() => !whyNoToggle && setEnabled([mod], disabled)}
                        ></button>
                      {/snippet}
                    </Tip>
                  </td>
                  <td class="name">
                    <div class="identity">
                      {#if logo}<img src={logo} alt="" width="32" height="32" loading="lazy" />{:else}<span class="logo" aria-hidden="true"></span>{/if}
                      <div class="about">
                        <div class="title-line">
                          <button class="expand" aria-expanded={expanded === mod.path} aria-controls="details-{domId(`${mod.key ?? mod.path}-${mod.file}`)}" onclick={() => expand(mod)}>{mod.name}</button>
                          {#if duplicatePaths.has(mod.path)}<span class="chip fail">Two versions</span>{/if}
                        </div>
                        <!-- The installed file and its update share a line, so a pack of hundreds stays scannable. -->
                        <div class="version">
                          {@render fileText(fileName(mod.path).replace(/\.disabled$/, ''))}
                          {#if status?.releaseType && status.releaseType !== 'release'}<span class="chip pending">{typeLabels[status.releaseType]}</span>{/if}
                          {#if lockedHere && held}
                            <span class="held" title={held.fileName}>Locked; {changedPart(fileName(mod.path).replace(/\.disabled$/, ''), held.fileName)} is available</span>
                          {:else if next}
                            <span class="next" title={next.fileName}>
                              <span class="visually-hidden">Update available: {next.fileName}</span>
                              <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M2 6h8M7 3l3 3-3 3" /></svg>
                              <span class="file" aria-hidden="true">{changedPart(fileName(mod.path).replace(/\.disabled$/, ''), next.fileName)}</span>
                              {#if next.releaseType !== 'release'}<span class="chip pending">{typeLabels[next.releaseType]}</span>{/if}
                            </span>
                          {/if}
                          {#if !lockedHere && waitingOnModrinth(status)}
                            {@const waiting = waitingOnModrinth(status)!}
                            <span class="held" title="{waiting.fileName} is on CurseForge, but Modrinth doesn't have it yet, so we hold the update until it does">
                              {changedPart(fileName(mod.path).replace(/\.disabled$/, ''), waiting.fileName)} isn't on Modrinth yet<span class="visually-hidden">, so we hold the update until it is</span>
                            </span>
                          {/if}
                        </div>
                        {#if mod.authors.length}<div class="byline muted">by {mod.authors.join(', ')}</div>{/if}
                      </div>
                    </div>
                  </td>
                  <td class="author muted">{mod.authors.join(', ')}</td>
                  <td class="update">
                    {#if next || change}
                      <!-- One button carries every state, so it keeps keyboard focus while you work down the list. -->
                      <Tip tip={change ? null : whyNoUpdate}>
                        {#snippet children(why)}
                          {@const shown = change && queuedButton(change, mod.name)}
                          <button
                            class:busy={shown?.busy}
                            class:cancels={shown?.cancels}
                            class:pressed={pressed === mod.path}
                            class:pointed={shown?.cancels && modUpdates.pointing === change?.serial}
                            aria-disabled={shown ? !shown.cancels : !!whyNoUpdate}
                            aria-describedby={why}
                            aria-label={shown?.ariaLabel ?? `Update ${mod.name}`}
                            title={shown?.title}
                            onpointerenter={() => point(change, true)}
                            onpointerleave={() => point(change, false)}
                            onfocus={(e) => pressed !== mod.path && e.currentTarget.matches(':focus-visible') && point(change, true)}
                            onblur={() => {
                              point(change, false)
                              if (pressed === mod.path) pressed = null
                            }}
                            onclick={() => {
                              if (change) return cancelChange(mod.path, mod.name)
                              if (whyNoUpdate || !next) return
                              pressed = mod.path
                              update([{ file: next.file, replaces: mod.path }])
                            }}
                          >{#if shown?.cancels}<span class="swap"><span>{shown.label}</span><span aria-hidden="true">Cancel</span></span>{:else}{shown?.label ?? 'Update'}{/if}</button>
                        {/snippet}
                      </Tip>
                    {:else if lockedHere && held}
                      <!-- aria-disabled keeps the button hoverable, so its title can say why it's off. -->
                      <button aria-disabled="true" aria-label="Update {mod.name}, locked" title="Unlock {mod.name} to update it">Update</button>
                    {/if}
                  </td>
                  <td class="locked">
                    <button
                      class="ghost icon lock"
                      aria-pressed={lockedHere}
                      onclick={() => setLocked(mod, !lockedHere)}
                      aria-label="Lock {mod.name} to this version"
                      title={lockedHere ? 'Locked to this version. Click to allow updates.' : 'Lock to this version'}
                    >
                      <svg viewBox="0 0 16 16" aria-hidden="true">
                        <rect x="3" y="7" width="10" height="7" rx="1.5" />
                        <path d={lockedHere ? 'M5.5 7V5a2.5 2.5 0 0 1 5 0v2' : 'M5.5 7V5a2.5 2.5 0 0 1 4.9-.7'} />
                      </svg>
                    </button>
                  </td>
                  <td class="trash">
                    <Tip tip={whyNoChange}>
                      {#snippet children(why)}
                        <button class="ghost icon remove" onclick={() => !whyNoChange && remove([mod])} disabled={removing === mod.path} aria-disabled={!!whyNoChange} aria-describedby={why} aria-label="Remove {mod.name}" title={whyNoChange ? undefined : `Move to the ${trashName}`}>
                          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.6 8.5h5.8l.6-8.5M7 7v4M9 7v4" /></svg>
                        </button>
                      {/snippet}
                    </Tip>
                  </td>
                </tr>
                {#if expanded === mod.path}
                  <tr class="details" id="details-{domId(`${mod.key ?? mod.path}-${mod.file}`)}">
                    <td colspan="7">
                      <div class="detail">
                        <div class="detail-head">
                          <div class="modes" role="group" aria-label="Show">
                            <button class="mode" aria-pressed={detail === 'versions'} onclick={() => expand(mod, 'versions')}>Versions</button>
                            <button class="mode" aria-pressed={detail === 'description'} onclick={() => expand(mod, 'description')}>Description</button>
                          </div>
                          {#if mod.url}<button class="link" onclick={() => openUrl(mod.url!)}>Open on {PLATFORM_NAMES[platformOf(mod.key)]}</button>{/if}
                        </div>
                        {#if detail === 'versions'}
                          {#if versionsError}
                            <p class="failed">{versionsError}</p>
                          {:else if !versions}
                            <p class="muted">Listing files on {PLATFORM_NAMES[platformOf(mod.key)]}.</p>
                          {:else if !versions.length}
                            <p class="muted">{mod.key ? `${PLATFORM_NAMES[platformOf(mod.key)]} has no files of ${mod.name} for ${game}.` : 'packraven knows no other versions of this file.'}</p>
                          {:else}
                            {@const reading = versions.find((f) => f.file === openChangelog) ?? versions[0]}
                            {@const changelog = changelogs[reading.file]}
                            {@const key = mod.key!}
                            <div class="versions">
                              <!-- svelte-ignore a11y_no_noninteractive_element_interactions (arrow keys move the selection between the buttons inside) -->
                              <ul class="files" aria-label="Versions of {mod.name}" onkeydown={(e) => pickByArrow(e, key)}>
                                {#each versions as file (file.file)}
                                  {@const reads = file === reading}
                                  <li class:reading={reads}>
                                    <button class="pick" tabindex={reads ? 0 : -1} aria-current={reads || undefined} aria-controls="changelog-{domId(key)}" onclick={() => showChangelog(key, file.file)}>
                                      <span class="visually-hidden">{file.fileName}</span>
                                      <span class="file" aria-hidden="true" title={file.fileName}>{versionLabel(file, versions)}</span>
                                      {#if file.releaseType !== 'release'}<span class="chip pending">{typeLabels[file.releaseType]}</span>{/if}
                                      {#if file.onModrinth === false}<span class="chip pending" title="Modrinth doesn't have this file yet, so we don't offer it as an update">Not on Modrinth yet<span class="visually-hidden">, so we don't offer it as an update</span></span>{/if}
                                      <span class="muted date">{date(file.fileDate)}</span>
                                    </button>
                                    {#if file.file === mod.file}
                                      <span class="muted current">Installed</span>
                                    {:else}
                                      {@const why = whyNoUpdate ?? (changeFor(mod.path) ? 'Wait for this change to finish' : null)}
                                      <Tip tip={why}>
                                        {#snippet children(described)}
                                          <button tabindex={reads ? 0 : -1} aria-disabled={!!why} aria-describedby={described} onclick={() => !why && update([{ file: file.file, replaces: mod.path }])} aria-label="{isNewer(file, mod.file, versions ?? []) ? 'Update to' : 'Roll back to'} {file.fileName}">{isNewer(file, mod.file, versions ?? []) ? 'Update' : 'Roll back'}</button>
                                        {/snippet}
                                      </Tip>
                                    {/if}
                                  </li>
                                {/each}
                              </ul>
                              <!-- svelte-ignore a11y_no_noninteractive_tabindex (a long changelog needs to scroll by keyboard even when it has no links) -->
                              <section class="changelog" id="changelog-{domId(key)}" aria-labelledby="changelog-title-{domId(key)}" tabindex="0">
                                <h3 id="changelog-title-{domId(key)}">Changelog of {@render fileText(reading.fileName)}</h3>
                                <p class="visually-hidden" role="status">{changelog === undefined ? `Loading the changelog of ${reading.fileName}.` : typeof changelog === 'string' ? `Showing the changelog of ${reading.fileName}.` : changelog.error}</p>
                                {#if changelog === undefined}
                                  <p class="muted" aria-hidden="true">Loading the changelog from {PLATFORM_NAMES[platformOf(key)]}.</p>
                                {:else if typeof changelog !== 'string'}
                                  <p class="failed" aria-hidden="true">{changelog.error}</p>
                                {:else}
                                  <RemoteHtml html={changelog} topHeading={4} />
                                {/if}
                              </section>
                            </div>
                          {/if}
                        {:else if descriptionError}
                          <p class="failed">{descriptionError}</p>
                        {:else if !mod.key}
                          <p class="muted">packraven doesn't know which project this file is from.</p>
                        {:else if descriptions[mod.key] === undefined}
                          <p class="muted">Loading the description from {PLATFORM_NAMES[platformOf(mod.key)]}.</p>
                        {:else}
                          <!-- svelte-ignore a11y_no_noninteractive_tabindex (a long description needs to scroll by keyboard even when it has no links) -->
                          <section class="description" aria-label="Description of {mod.name}" tabindex="0">
                            <RemoteHtml html={descriptions[mod.key]} topHeading={3} />
                          </section>
                        {/if}
                      </div>
                    </td>
                  </tr>
                {/if}
              {:else}
                <tr>
                  <td colspan="7" class="muted">
                    {#if search.trim()}
                      {@const elsewhere = (Object.keys(kindLabels) as Kind[]).filter((k) => k !== kindFilter && matchCounts.get(k))}
                      No {kindLabels[kindFilter].toLowerCase()} match "{search.trim()}".
                      {#each elsewhere as kind (kind)}
                        <button class="link" onclick={() => (kindFilter = kind)}>Show {matchCounts.get(kind)} in {kindLabels[kind]}</button>{' '}
                      {/each}
                      <button class="link" onclick={() => (search = '')}>Clear the search</button>
                    {:else}
                      This pack has no {kindLabels[kindFilter].toLowerCase()} yet.
                      {#each ['curseforge', 'modrinth'] as const as platform (platform)}
                        <button class="link" onclick={() => startAdding(platform)}>Add from {PLATFORM_NAMES[platform]}</button>{' '}
                      {/each}
                    {/if}
                  </td>
                </tr>
              {/each}
            {/if}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
</div>

<DisableDialog request={disabling} onconfirm={confirmDisable} oncancel={() => (disabling = null)} />
<UpdateReview request={reviewing} onconfirm={confirmUpdates} oncancel={() => (reviewing = null)} />

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} label={menu.label} items={menu.items} scroller={tableRegion} onclose={() => (menu = null)} />
{/if}

<style>
  .mods {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-height: 0;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    justify-content: space-between;
    gap: 12px 24px;
  }

  /* The title takes the room the actions leave and wraps its own text, so a longer summary doesn't
     push the actions onto a row of their own. */
  .title {
    flex: 1 1 20rem;
    min-width: 0;
    display: grid;
    gap: 4px;
  }

  .run-status {
    flex: 1 1 0;
    min-width: 0;
    align-self: center;
    display: flex;
    justify-content: end;
    gap: 8px;
    white-space: nowrap;
  }

  .run-status span {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .head p {
    max-width: 64ch;
  }

  .tab-row {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    justify-content: space-between;
    gap: 8px 16px;
    border-bottom: 1px solid var(--line);
  }

  .add-from {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 6px;
  }

  .add-from button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .add-from svg {
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.6;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  /* Category and sort wrap below the type chips together, so they never split across two rows. */
  .elsewhere {
    display: block;
    margin-top: 4px;
    color: var(--warn);
  }

  .refine {
    flex: 1 1 auto;
    display: flex;
    align-items: center;
    justify-content: end;
    gap: 16px;
  }

  /* Wide enough for category names like "Adventure and RPG", which a narrower menu cuts off. */
  .refine select {
    width: 20ch;
    max-width: 100%;
  }

  .sort {
    display: flex;
    align-items: center;
    gap: 8px;
    white-space: nowrap;
  }

  /* The search field keeps room on the right for its clear button, so it needs more width than
     the placeholder's text alone. */
  .find {
    flex: 0 1 260px;
    min-width: min(100%, 15rem);
    margin-bottom: 6px;
  }

  .tab-count,
  .filter-count {
    margin-left: 6px;
    font-weight: 400;
    font-variant-numeric: tabular-nums;
  }

  .panel {
    container-type: inline-size;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-height: 0;
  }

  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px 16px;
  }

  /* The launch and update groups wrap as units, and the wider gap between them keeps running the
     game apart from changing mods. */
  .actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: end;
    gap: 8px 20px;
  }

  .update-actions {
    display: flex;
    gap: 8px;
  }



  .filters {
    display: flex;
    flex-wrap: wrap;
    max-width: 100%;
    padding: 3px;
    gap: 3px;
    background: var(--paper);
    border: 1px solid var(--line);
    border-radius: var(--radius-control);
  }

  .filters button {
    font-weight: 400;
    padding: 5px clamp(8px, 1.5vw, 14px);
    color: var(--muted);
    background: transparent;
    border-color: transparent;
  }

  .filters button:hover:not(.on) {
    color: var(--text);
    background: var(--control-hi);
  }

  .filters button.on {
    color: var(--text);
    font-weight: 700;
    border-color: var(--control-edge);
    background: var(--control);
  }

  .has-updates {
    display: inline-block;
    width: 7px;
    height: 7px;
    margin-left: 6px;
    vertical-align: 0.15em;
    border-radius: 50%;
    background: var(--ochre-ink);
  }

  .head .channel {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    font-size: var(--step--1);
  }

  .head .channel select {
    padding: 2px 6px;
    font-size: var(--step--1);
  }

  .select {
    width: 1%;
    padding-inline: 12px 4px;
  }

  .select input {
    display: block;
    margin: 0;
  }

  .row.picked td {
    background: var(--here);
  }

  .bulk {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 6px 8px 6px 12px;
    background: var(--here);
    border: 1px solid var(--here-border);
    border-radius: var(--radius-control);
  }

  .bulk-count {
    margin-right: 6px;
    font-weight: 700;
    color: var(--ochre-ink);
    font-variant-numeric: tabular-nums;
  }

  .bulk .clear {
    margin-left: auto;
  }

  .danger-quiet:hover:not(:disabled, [aria-disabled='true']) {
    color: var(--fail);
  }


  .log summary {
    cursor: pointer;
    color: var(--muted);
    margin-bottom: 8px;
  }

  .skeleton span {
    display: block;
    height: 0.9em;
    width: 70%;
    margin: 4px 0;
    border-radius: var(--radius-small);
    background: var(--line);
    animation: pulse 1.4s ease-in-out infinite;
  }

  .skeleton span.short {
    width: 35%;
    opacity: 0.6;
  }

  @keyframes pulse {
    50% {
      opacity: 0.45;
    }
  }

  /* We let the table fill the rest of the view, so it scrolls without a page scroll around it. */
  .table {
    flex: 1;
    min-height: 240px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: var(--radius-panel);
  }

  table {
    width: 100%;
    border-collapse: collapse;
  }

  th {
    position: sticky;
    top: 0;
    z-index: 1;
    text-align: left;
    font-size: var(--step--1);
    color: var(--muted);
    font-weight: 400;
    background: var(--surface);
    padding: 0;
    white-space: nowrap;
    /* A nominal height lets the sort buttons fill the row with height: 100%. */
    height: 1px;
  }

  th.trash {
    padding: 4px 8px;
  }

  th.enabled,
  th.locked,
  th.trash {
    width: 1%;
  }

  /* The whole header cell sorts, out to the column edges, not just its label. */
  .sort {
    display: flex;
    align-items: center;
    gap: 4px;
    width: 100%;
    height: 100%;
    padding: 8px;
    font: inherit;
    color: inherit;
    background: transparent;
    border: 0;
    border-radius: 0;
  }

  th.update .sort {
    justify-content: flex-end;
  }

  .sort:hover:not(:disabled) {
    color: var(--text);
    background: transparent;
  }

  .sort.sorted {
    color: var(--text);
    font-weight: 700;
  }

  .sort svg {
    width: 10px;
    height: 10px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
    opacity: 0;
  }

  .sort.sorted svg,
  .sort:hover svg,
  .sort:focus-visible svg {
    opacity: 1;
  }

  /* The switch and padlock columns are too narrow for words, so their headers draw the control. */
  th.enabled .sort::before {
    content: '';
    width: 20px;
    height: 11px;
    border: 1.5px solid currentColor;
    border-radius: 999px;
  }

  th.locked .sort::before {
    content: '';
    width: 13px;
    height: 13px;
    background: currentColor;
    mask: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16' fill='none' stroke='black' stroke-width='1.5'%3E%3Crect x='3' y='7' width='10' height='7' rx='1.5'/%3E%3Cpath d='M5.5 7V5a2.5 2.5 0 0 1 5 0v2'/%3E%3C/svg%3E") center / contain no-repeat;
  }

  td {
    padding: 8px;
    border-top: 1px solid var(--line);
    vertical-align: middle;
  }

  .row {
    cursor: pointer;
  }

  .row:hover td,
  tr.open td {
    background: var(--paper);
  }

  .row.off .identity img,
  .row.off .logo {
    opacity: 0.5;
  }

  .row.off .expand {
    color: var(--muted);
    font-weight: 400;
  }

  .row.off .next {
    color: var(--muted);
  }

  .identity {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .identity img,
  .logo {
    flex: none;
    width: 32px;
    height: 32px;
    border-radius: var(--radius-small);
    object-fit: cover;
  }

  .logo {
    background: var(--line);
  }

  .about {
    display: grid;
    gap: 1px;
    min-width: 0;
  }

  .title-line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px 8px;
  }

  .expand {
    padding: 0;
    font: inherit;
    font-weight: 700;
    color: var(--text);
    text-align: left;
    background: transparent;
    border: 0;
  }

  .expand:hover:not(:disabled) {
    background: transparent;
    text-decoration: underline;
    text-underline-offset: 0.18em;
  }

  .version {
    color: var(--muted);
  }

  td .file {
    margin-right: 6px;
  }

  .version {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0 6px;
  }

  .version .file {
    margin-right: 0;
  }

  .held {
    font-size: var(--step--1);
  }

  .next {
    color: var(--ochre-ink);
  }

  .next svg {
    width: 12px;
    height: 12px;
    margin-right: 4px;
    vertical-align: -1px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.6;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .author,
  .byline {
    font-size: var(--step--1);
  }

  .author {
    min-width: 9ch;
  }

  .byline {
    display: none;
  }

  /* A narrow table folds the author column under each mod rather than scrolling sideways. */
  @container (max-width: 600px) {
    th.author,
    td.author {
      display: none;
    }

    .byline {
      display: block;
    }
  }

  /* Half of a split at the minimum window still has to fit every control column. */
  @container (max-width: 440px) {
    td {
      padding-inline: 4px;
    }

    .update {
      white-space: normal;
    }
  }

  /* In a short window a second scroller inside the page would show only a few rows, so the
     table grows with the page instead and its header sticks to the page's scroller. */
  @media (max-height: 700px) {
    .mods,
    .panel {
      flex: none;
    }

    .table {
      flex: none;
      min-height: 0;
      overflow: visible;
    }

    /* The page's scroller has 28px of top padding, and rows would show through it above the
       stuck header. The shadow paints that band in the page color, and the matching gap keeps
       the shadow off the toolbar before the header sticks. */
    .panel {
      gap: 28px;
    }

    th {
      box-shadow: 0 -28px 0 1px var(--paper);
    }

    th:first-child {
      border-top-left-radius: var(--radius-panel);
    }

    th:last-child {
      border-top-right-radius: var(--radius-panel);
    }
  }

  /* Every control column stays even in half of a split, so there the table scrolls sideways in its
     own frame rather than widening the view. This comes after the short-window rule on purpose. */
  @container (max-width: 440px) {
    .table {
      overflow-x: auto;
    }
  }

  .update {
    text-align: right;
    white-space: nowrap;
  }

  /* Wide enough for the busy label, so queueing an update doesn't shift the columns. A narrow pane,
     such as a zoomed-in minimum window or half a split, has no room to spare for it. */
  @container pane (min-width: 532px) {
    td.update button {
      min-width: 8.5em;
    }
  }

  button.icon {
    display: grid;
    place-items: center;
    padding: 6px;
  }

  button.icon svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.25;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .lock[aria-pressed='false'],
  .remove {
    color: var(--muted);
  }

  .row:hover .lock[aria-pressed='false'],
  .lock[aria-pressed='false']:focus-visible {
    color: var(--text);
  }

  .remove:hover:not(:disabled),
  .remove:focus-visible {
    color: var(--fail);
  }

  .lock[aria-pressed='true'] {
    color: var(--ochre-ink);
  }

  .lock[aria-pressed='true'] rect {
    fill: currentColor;
  }

  .switch {
    position: relative;
    display: block;
    width: 30px;
    height: 18px;
    padding: 0;
    border-radius: 999px;
    border: 1px solid var(--control-edge);
    background: var(--line-strong);
  }

  .switch::after {
    content: '';
    position: absolute;
    top: 2px;
    left: 2px;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--field);
    transition: transform 120ms ease-out;
  }

  .switch[aria-checked='true'] {
    border-color: var(--forest);
    background: var(--forest);
  }

  .switch[aria-checked='true']::after {
    transform: translateX(12px);
  }

  .switch:hover:not(:disabled) {
    border-color: var(--forest-hi);
  }

  .switch[aria-checked='true']:hover:not(:disabled) {
    background: var(--forest-hi);
  }

  .details td {
    border-top: 0;
    padding: 4px 12px 14px 58px;
    background: var(--paper);
    cursor: auto;
  }

  .detail-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 8px 16px;
    margin-bottom: 8px;
  }

  .modes {
    width: fit-content;
  }

  .detail {
    container-type: inline-size;
  }

  .versions {
    display: grid;
    gap: 12px;
  }

  .files,
  .changelog,
  .description {
    min-width: 0;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: var(--radius-panel);
    background: var(--field);
  }

  .files {
    list-style: none;
    margin: 0;
    padding: 4px;
    max-height: 220px;
  }

  .files li {
    display: flex;
    align-items: center;
    gap: 6px 10px;
    padding: 2px 4px 2px 0;
    border-radius: var(--radius-control);
  }

  .files li.reading {
    background: var(--here);
  }

  .pick {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px 10px;
    padding: 6px 8px;
    font: inherit;
    color: inherit;
    text-align: left;
    background: transparent;
    border: 0;
    border-radius: var(--radius-control);
  }

  .pick:hover:not(:disabled) {
    background: transparent;
  }

  .files li:not(.reading):has(.pick:hover) {
    background: var(--control-hi);
  }

  .pick .file {
    margin-right: 0;
  }

  .pick .date {
    font-size: var(--step--1);
    font-variant-numeric: tabular-nums;
  }

  .files .current {
    padding-inline: 8px;
    white-space: nowrap;
  }

  .changelog {
    padding: 14px 18px;
    max-height: clamp(240px, 45vh, 560px);
  }

  .changelog h3 {
    margin: 0 0 10px;
    font-size: var(--step--1);
    font-weight: 400;
    color: var(--muted);
  }

  .changelog h3 .file {
    color: var(--text);
  }

  /* With room for both, the changelog sits beside the versions and shares their height, so a long
     one reads in place instead of scrolling inside the list. The changelog stops at a reading
     width, and a wide window gives the rest to the list. */
  @container (min-width: 760px) {
    .versions {
      grid-template-columns: minmax(280px, 1fr) minmax(0, 84ch);
    }

    .files,
    .changelog {
      max-height: clamp(260px, 50vh, 620px);
    }
  }

  .details p {
    margin: 4px 0;
  }

  .description {
    max-width: 90ch;
    max-height: clamp(320px, 60vh, 760px);
    padding: 14px 18px;
  }
</style>
