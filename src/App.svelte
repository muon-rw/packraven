<script lang="ts">
  import { loaderName } from './lib/loader'
  import Notice from './components/Notice.svelte'
  import { onMount, tick } from 'svelte'
  import { listen } from '@tauri-apps/api/event'
  import { api, errorText, type Overview, type PackList, type Platform } from './lib/api'
  import { minecraft, refreshAccount } from './lib/minecraft.svelte'
  import { job, jobs, onJobEnd, serverPack, setJobPack, startListening, testState } from './lib/jobs.svelte'
  import Changes from './views/Changes.svelte'
  import Server from './views/Server.svelte'
  import Mods from './views/Mods.svelte'
  import { updateCount } from './lib/modUpdates.svelte'
  import TestRun from './views/TestRun.svelte'
  import Publish from './views/Publish.svelte'
  import SettingsView from './views/Settings.svelte'
  import AddPack from './views/AddPack.svelte'
  import Git from './views/Git.svelte'
  import { nav, revealAnchor } from './lib/nav.svelte'
  import Releases from './views/Releases.svelte'
  import PackSettings from './views/PackSettings.svelte'
  const loadFiles = () => import('./views/Files.svelte')
  import { dirtyCount, startWatching } from './lib/workspace.svelte'
  import { refreshSelfUpdate, selfUpdate, startSelfUpdates, updateWaiting } from './lib/selfUpdate.svelte'
  import DefenderNotice from './components/DefenderNotice.svelte'
  import GitNotice from './components/GitNotice.svelte'
  import IgnoreDialog from './components/IgnoreDialog.svelte'
  import UpdateChangelogDialog from './components/UpdateChangelogDialog.svelte'
  import { ask, message } from '@tauri-apps/plugin-dialog'
  import PackSwitcher from './components/PackSwitcher.svelte'
  import RavenMark from './components/RavenMark.svelte'
  import ThemeToggle from './components/ThemeToggle.svelte'
  import VersionEditor from './components/VersionEditor.svelte'
  import { hostsOf, modCountsSummary, notOnHostsSummary } from './lib/modsync'
  import { commandKey, isMac, shortcut } from './lib/platform'
  import Pane from './components/Pane.svelte'
  import Splitter from './components/Splitter.svelte'
  import ContextMenu from './components/ContextMenu.svelte'
  import type { MenuItem } from './lib/menu'
  import { clamp } from './lib/storedSplit.svelte'
  import { closePane, focusedView, isSplit, layout, only, openBeside, paneSplit, placeInPane, show, showFrom, splitCompanion, swapPanes, titles, toggleSplit, views, type View } from './lib/panes.svelte'

  type Tone = 'ok' | 'warn' | 'bad' | 'busy' | 'idle'
  type NavItem = { id: View; title: string; key?: string; alias?: string; status?: string; tone?: Tone; count?: number; countText?: string }

  // In dev builds, `?view=publish` opens straight to a view, so screenshot scripts don't have to click there.
  // `&beside=git` opens a second view in a split.
  function initialLayout() {
    const params = import.meta.env.DEV ? new URLSearchParams(location.search) : null
    const pick = (name: string) => views.find((v) => v === params?.get(name))
    only(pick('view') ?? 'mods')
    const beside = pick('beside')
    if (beside) openBeside(beside)
  }

  initialLayout()
  let view = $derived(focusedView())
  let split = $derived(isSplit())
  let overview = $state<Overview | null>(null)
  let loadError: string | null = $state(null)
  // Files and Git only need the pack folder, and they're how you fix a pack we can't load, like
  // rolling back a mods.lock.json from a newer packraven or fixing packraven.toml.
  const OPEN_WITHOUT_PACK: View[] = ['settings', 'files', 'git']
  let packs = $state<PackList | null>(null)
  let hookMessage: string | null = $state(null)
  let hookFailed = $state(false)
  let hooksDismissed = $state(false)
  let account = $derived(minecraft.account)
  // When a renewal fails at launch, the backend signs you out, so we read the account again.
  for (const name of ['client', 'test'] as const) onJobEnd(name, (phase) => phase === 'failed' && refreshAccount())

  // We refresh when files, git refs or the mod list change, so the timer only catches changes with no
  // event. A test client can rewrite configs every second, and each refresh runs git several times,
  // so we refresh at most every 5 seconds and run one more after the last change.
  const FALLBACK_POLL_MS = 30_000
  const REFRESH_GAP_MS = 5_000
  let refreshing = false
  let queued = false
  let lastRefresh = 0
  let pending: ReturnType<typeof setTimeout> | null = null

  // We skip while the window is hidden in the tray and catch up when it's shown again.
  function poll() {
    if (document.hidden) return
    if (refreshing) queued = true
    if (refreshing || pending) return
    pending = setTimeout(
      () => {
        pending = null
        if (refreshing) queued = true
        else refresh()
      },
      Math.max(0, lastRefresh + REFRESH_GAP_MS - Date.now()),
    )
  }

  async function refresh() {
    refreshing = true
    lastRefresh = Date.now()
    try {
      await Promise.all([load(), refreshSelfUpdate()])
    } finally {
      refreshing = false
      if (queued) {
        queued = false
        poll()
      }
    }
  }

  let resyncing = false

  // The file watcher can miss changes, so we check the mods folder again when you come back to packraven.
  async function resync() {
    if (resyncing || document.hidden) return
    resyncing = true
    try {
      await api.resyncModList(false)
    } catch {
      // The rail already reports a mod list that can't update.
    } finally {
      resyncing = false
    }
  }

  function returned() {
    resync()
    poll()
  }

  async function load() {
    packs = await api.listPacks().catch(() => packs)
    // We set it in the same update as packs, so views remounted for the new pack never see the last pack's jobs.
    setJobPack(packs?.active ?? null)
    if (packs && !packs.active) {
      overview = null
      loadError = null
      only(view === 'settings' ? 'settings' : 'add')
      return
    }
    try {
      overview = await api.overview()
      loadError = null
    } catch (error) {
      overview = null
      loadError = errorText(error)
    }
  }

  onMount(() => {
    startListening()
    startWatching()
    refreshAccount()
    const stopSelfUpdates = startSelfUpdates()
    // When CurseForge approves a jar you uploaded, nothing changes on disk, so while one is waiting we
    // check again on the timer. In the background we ask CurseForge about it at most once a minute.
    const timer = setInterval(() => {
      if (overview?.unhosted.length) resync()
      poll()
    }, FALLBACK_POLL_MS)
    const unlisten = listen<{ kind: string }>('test-event', ({ payload }) => {
      if (payload.kind.endsWith('-started') || payload.kind.endsWith('-stopped')) refresh()
    })
    const unlistenChanges = ['mod-list-changed', 'fs-change', 'git-change'].map((event) => listen(event, poll))
    window.addEventListener('focus', returned)
    document.addEventListener('visibilitychange', returned)
    resync()
    return () => {
      stopSelfUpdates()
      clearInterval(timer)
      if (pending) clearTimeout(pending)
      unlisten.then((stop) => stop())
      for (const unlistenChange of unlistenChanges) unlistenChange.then((stop) => stop())
      window.removeEventListener('focus', returned)
      document.removeEventListener('visibilitychange', returned)
    }
  })

  $effect(() => {
    for (const name in jobs) void jobs[name].phase
    refresh()
  })

  function killWarning(server: boolean, client: boolean) {
    if (server && client) return "The test server and client are still running. If you switch packs, packraven kills them, and the server doesn't save its world."
    if (server) return "The test server is still running. If you switch packs, packraven kills it without saving its world."
    return 'The test client is still running. If you switch packs, packraven kills it.'
  }

  async function switchPack(dir: string) {
    const leaving = dir !== packs?.active
    const killTests = leaving && !!(overview?.serverRunning || overview?.clientRunning)
    if (killTests) {
      const confirmed = await ask(killWarning(!!overview?.serverRunning, !!overview?.clientRunning), {
        title: 'Test still running',
        kind: 'warning',
        okLabel: 'Kill and switch',
        cancelLabel: 'Keep running',
      })
      if (!confirmed) return
    }
    const unsaved = dirtyCount()
    if (unsaved && leaving) {
      const confirmed = await ask(`${unsaved === 1 ? 'A file has' : `${unsaved} files have`} unsaved changes. If you switch packs, packraven closes ${unsaved === 1 ? 'it' : 'them'} without saving.`, {
        title: 'Unsaved changes',
        kind: 'warning',
        okLabel: 'Switch anyway',
        cancelLabel: 'Keep editing',
      })
      if (!confirmed) return
    }
    try {
      await api.switchPack(dir, killTests)
      // A split you set up is a layout for your work, not for one pack, so we keep it.
      if (!split) show('mods')
      hookMessage = null
      hooksDismissed = false
      await refresh()
    } catch (error) {
      await message(errorText(error), { title: "Couldn't switch packs", kind: 'error' })
    }
  }

  async function updateHooks() {
    try {
      await api.updateHooks()
      hookFailed = false
      hookMessage = 'Hooks updated. Commit the .githooks changes so other machines get them.'
      await refresh()
    } catch (error) {
      hookFailed = true
      hookMessage = `Couldn't update the hooks: ${errorText(error)}`
    }
  }

  let addonsOpen = $state(false)
  let addonRows = $derived(
    overview
      ? ([
          ['Mods', overview.addons.mods],
          ['Resource packs', overview.addons.resourcePacks],
          ['Shader packs', overview.addons.shaderPacks],
          ['Data packs', overview.addons.dataPacks],
          ['Other', overview.addons.other],
        ] as const).filter(([label, count]) => count > 0 || label !== 'Other')
      : [],
  )

  let releasesStatus = $derived(
    [
      overview?.releasesMissingServerPack ? `${overview.releasesMissingServerPack} ${overview.releasesMissingServerPack === 1 ? 'release' : 'releases'} without a server pack` : null,
      overview?.releasesMissingModrinth ? `${overview.releasesMissingModrinth} not on Modrinth yet` : null,
    ]
      .filter(Boolean)
      .join(', ') || undefined,
  )

  let publishesTo = $derived<Platform[]>([...(overview && overview.projectId > 0 ? ['curseforge' as const] : []), ...(overview?.modrinthProjectId ? ['modrinth' as const] : [])])

  let modListStatus = $derived(
    overview?.modListError
      ? "Couldn't update the mod list"
      : overview?.unhosted.length
        ? notOnHostsSummary(overview.unhosted, hostsOf(overview))
        : overview?.watchError
          ? "Can't watch the pack folder"
          : undefined,
  )
  // Publish is where you decide what each platform's pack carries, so it flags the files waiting on you.
  let hostingStatus = $derived(
    overview?.modrinthError
      ? "Couldn't check Modrinth"
      : overview?.notOnCurseforge.length
        ? notOnHostsSummary(overview.notOnCurseforge, 'CurseForge')
        : overview?.notOnModrinth.length
          ? notOnHostsSummary(overview.notOnModrinth, 'Modrinth')
          : undefined,
  )
  let incomingSummary = $derived(overview?.incoming ? modCountsSummary(overview.incoming) : null)

  let testStatus = $derived(
    testState.players.length
      ? `${testState.players.join(', ')} joined`
      : [
          testState.serverReady ? 'Server up' : overview?.serverRunning ? 'Server starting' : null,
          overview?.clientRunning ? 'Client running' : null,
        ]
          .filter(Boolean)
          .join(', ') || undefined,
  )

  let publishPhase = $derived(job('publish').phase)
  let modsPhase = $derived(job('mods').phase)
  let updates = $derived(updateCount(packs?.active ?? null))

  let groups = $derived<{ label?: string; items: NavItem[] }[]>([
    {
      label: 'Work',
      items: [
        {
          id: 'mods',
          title: 'Mods',
          key: '1',
          status: { running: 'Updating', failed: 'Update failed', done: undefined, idle: undefined }[modsPhase],
          tone: ({ running: 'busy', failed: 'bad', done: 'idle', idle: 'idle' } as const)[modsPhase],
          count: updates,
          countText: `${updates} ${updates === 1 ? 'update' : 'updates'} available`,
        },
        { id: 'files', title: 'Files', key: '2', alias: 'E' },
        {
          id: 'server',
          title: 'Server',
          key: '3',
          status: serverPack.changedSinceBuild ? 'Changed since you last built the server pack' : undefined,
          tone: serverPack.changedSinceBuild ? 'warn' : 'idle',
        },
        {
          id: 'test',
          title: 'Run',
          key: '4',
          status: testStatus,
          tone: overview?.serverRunning && !testState.serverReady ? 'busy' : testStatus ? 'ok' : 'idle',
        },
        {
          id: 'git',
          title: 'Git',
          key: '5',
          alias: 'G',
          count: overview?.changed.length,
          countText: `${overview?.changed.length} uncommitted ${overview?.changed.length === 1 ? 'file' : 'files'}`,
        },
        {
          id: 'changes',
          title: 'Changes',
          key: '6',
          status: !overview ? 'Instance not found' : modListStatus,
          tone: !overview ? 'bad' : modListStatus ? 'warn' : 'idle',
        },
      ],
    },
    {
      label: 'Release',
      items: [
        {
          id: 'publish',
          title: 'Publish',
          key: '7',
          status: { running: 'Uploading', failed: 'Publish failed', done: 'Published', idle: hostingStatus }[publishPhase],
          tone: publishPhase === 'idle' && hostingStatus ? 'warn' : ({ running: 'busy', failed: 'bad', done: 'ok', idle: 'idle' } as const)[publishPhase],
        },
        {
          id: 'releases',
          title: 'Releases',
          key: '8',
          status: releasesStatus,
          tone: releasesStatus ? 'warn' : undefined,
        },
      ],
    },
    { items: [{ id: 'pack', title: 'Pack settings', key: '9' }] },
  ])

  $effect(() => {
    if (job('server-pack').phase === 'done' || publishPhase === 'done') serverPack.changedSinceBuild = false
  })

  // A link inside a view asks for nav.view, and in a split it opens in the other pane.
  $effect(() => {
    if (!nav.view) return
    showFrom(nav.view, layout.focused)
    nav.view = null
    if (nav.anchor) revealAnchor(nav.anchor)
    nav.anchor = null
  })

  let navItems = $derived(groups.flatMap((group) => group.items))

  function onKey(event: KeyboardEvent) {
    if (event.key === 'F6' && split && !event.ctrlKey && !event.metaKey && !event.altKey) {
      event.preventDefault()
      layout.focused = 1 - layout.focused
      if (tabbed) document.getElementById(`pane-${layout.focused}-tab`)?.focus()
      return
    }
    if (!(event.ctrlKey || event.metaKey) || event.altKey) return
    if (event.code === 'Backslash' && !event.shiftKey) {
      if (!packs?.active) return
      event.preventDefault()
      toggleSplit()
      return
    }
    if (event.shiftKey) {
      // Shift changes event.key for the digits, so we match the physical key.
      const digit = /^Digit([1-9])$/.exec(event.code)?.[1]
      const beside = digit && packs?.active ? navItems.find((item) => item.key === digit) : undefined
      if (beside) {
        event.preventDefault()
        openBeside(beside.id)
        return
      }
      const key = event.key.toLowerCase()
      if (!packs?.active || (key !== 'f' && key !== 'h')) return
      event.preventDefault()
      nav.textSearch = key === 'h' ? 'replace' : 'find'
      show('files')
      return
    }
    const key = event.key.toUpperCase()
    // Files and Git keep their old letter shortcuts as aliases.
    const item = packs?.active ? navItems.find((item) => item.key === key || item.alias === key) : undefined
    if (item) show(item.id)
    else if (event.key === ',') show('settings')
    else return
    event.preventDefault()
  }

  function onRailClick(event: MouseEvent, id: View) {
    if (suppressRailClick) return
    if (commandKey(event) && packs?.active) openBeside(id)
    else show(id)
  }

  const RAIL_DRAG_THRESHOLD = 5
  let panesEl = $state<HTMLDivElement>()
  let railPress: { x: number; y: number; id: View; pointerId: number; target: HTMLElement } | null = null
  let railGhost = $state<{ x: number; y: number; id: View } | null>(null)
  let dropSide = $state<0 | 1 | null>(null)
  let suppressRailClick = false

  function onRailPointerDown(event: PointerEvent, id: View) {
    if (event.button !== 0 || !packs?.active || view === 'add') return
    railPress = { x: event.clientX, y: event.clientY, id, pointerId: event.pointerId, target: event.currentTarget as HTMLElement }
  }

  function onRailPointerMove(event: PointerEvent) {
    if (!railPress) return
    if (!railGhost) {
      if (Math.hypot(event.clientX - railPress.x, event.clientY - railPress.y) < RAIL_DRAG_THRESHOLD) return
      railPress.target.setPointerCapture(railPress.pointerId)
    }
    railGhost = { x: event.clientX, y: event.clientY, id: railPress.id }
    const rect = panesEl?.getBoundingClientRect()
    if (!rect || event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) {
      dropSide = null
      return
    }
    const splitEdge = paneMode === 'split' ? rect.left + leftWidth : rect.left + rect.width / 2
    dropSide = event.clientX < splitEdge ? 0 : 1
  }

  function onRailPointerUp() {
    railPress = null
    if (!railGhost) return
    const { id } = railGhost
    const side = dropSide
    railGhost = null
    dropSide = null
    suppressRailClick = true
    setTimeout(() => (suppressRailClick = false))
    if (side !== null) placeInPane(id, side)
  }

  function cancelRailDrag() {
    const press = railPress
    const wasDragging = !!railGhost
    railPress = null
    railGhost = null
    dropSide = null
    if (wasDragging) {
      suppressRailClick = true
      setTimeout(() => (suppressRailClick = false))
      if (press?.target.hasPointerCapture(press.pointerId)) press.target.releasePointerCapture(press.pointerId)
    }
  }

  function dropLabel(side: 0 | 1) {
    const unit = splitTotal > 0 && splitTotal < 2 * PANE_MIN ? 'tab' : 'pane'
    const label = `${side === 0 ? 'Left' : 'Right'} ${unit}`
    return !split && railGhost?.id === layout.panes[0] ? `${label} · split with ${titles[splitCompanion()]}` : label
  }

  let railMenu = $state<{ x: number; y: number; items: MenuItem[]; scroller: EventTarget | null } | null>(null)

  function onRailMenu(event: MouseEvent, id: View, key?: string) {
    event.preventDefault()
    cancelRailDrag()
    if (!packs?.active) return
    const at = layout.panes.indexOf(id)
    const other = 1 - layout.focused
    const besideShortcut = key ? shortcut(`Ctrl+Shift+${key}`) : undefined
    const items: MenuItem[] = split
      ? [
          ...(at < 0
            ? [0, 1].map((pane) => ({ label: `Open in ${pane ? 'right' : 'left'} pane`, shortcut: pane === other ? besideShortcut : undefined, run: () => show(id, pane) }))
            : [{ label: 'Close pane', run: () => closeAndFocus(at) }]),
          'separator',
          { label: 'Swap sides', run: swapPanes },
          { label: `Close split, keep ${titles[view]}`, shortcut: shortcut('Ctrl+\\'), run: toggleSplit },
        ]
      : at < 0
        ? [
            { label: 'Open', run: () => show(id) },
            { label: 'Open beside', shortcut: besideShortcut, run: () => openBeside(id) },
          ]
        : [{ label: `Split with ${titles[splitCompanion()]}`, shortcut: shortcut('Ctrl+\\'), run: toggleSplit }]
    openMenu(event, items)
  }

  function onPaneMenu(event: MouseEvent, pane: number) {
    event.preventDefault()
    openMenu(event, [
      { label: 'Swap sides', run: swapPanes },
      { label: `Close ${titles[layout.panes[pane]]}`, run: () => closeAndFocus(pane) },
    ])
  }

  function openMenu(event: MouseEvent, items: MenuItem[]) {
    // The keyboard menu key fires contextmenu without pointer coordinates.
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
    const fromKeyboard = !event.clientX && !event.clientY
    railMenu = { x: fromKeyboard ? rect.left + 24 : event.clientX, y: fromKeyboard ? rect.bottom : event.clientY, items, scroller: event.currentTarget }
  }

  // The close button leaves with its pane, so we hand focus to the pane that stays.
  async function closeAndFocus(pane: number) {
    closePane(pane)
    await tick()
    document.getElementById('pane-0')?.focus({ preventScroll: true })
  }

  const openBesideHint = `${isMac ? '⌘-click' : 'Ctrl+click'} or drag to open beside`

  function railTitle(status?: string) {
    return status ?? (packs?.active && !split ? openBesideHint : undefined)
  }

  function sideOf(id: View) {
    const at = layout.panes.indexOf(id)
    return split && at >= 0 ? (at ? 'right' : 'left') : null
  }

  const PANE_GAP = 1
  // Views stay usable down to about this width. A narrower split, as in a zoomed-in minimum
  // window, shows its panes as tabs instead.
  const PANE_MIN = 360
  let panesWidth = $state(0)
  let splitTotal = $derived(panesWidth - PANE_GAP)
  let tabbed = $derived(split && panesWidth > 0 && splitTotal < 2 * PANE_MIN)
  let paneMode = $derived<'single' | 'split' | 'tabs'>(!split ? 'single' : tabbed ? 'tabs' : 'split')
  let leftWidth = $derived(clamp(Math.round((paneSplit.value ?? 0.5) * splitTotal), PANE_MIN, splitTotal - PANE_MIN))

  // A split too narrow to show side by side becomes tabs, and then you'd lose sight of the view you came from anyway.
  function openBesideIfRoom(view: View, pane: number) {
    if (splitTotal >= 2 * PANE_MIN) openBeside(view)
    else showFrom(view, pane)
  }

  function onTabKey(event: KeyboardEvent) {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return
    event.preventDefault()
    layout.focused = 1 - layout.focused
    document.getElementById(`pane-${layout.focused}-tab`)?.focus()
  }
</script>

<svelte:window
  onkeydown={(e) => {
    if (e.key === 'Escape' && railGhost) {
      e.preventDefault()
      cancelRailDrag()
    } else {
      onKey(e)
    }
  }}
  onpointermove={onRailPointerMove}
  onpointerup={onRailPointerUp}
  onpointercancel={cancelRailDrag}
  onblur={cancelRailDrag}
/>

<div class="shell" class:dragging-rail={!!railGhost}>
  <aside class="rail">
    <div class="brand"><RavenMark /><span>packraven</span><ThemeToggle /></div>

    <section class="pack" aria-label="Current pack">
      <div class="identity">
        <PackSwitcher list={packs} name={overview?.name ?? (packs?.active ? 'Pack unavailable' : 'No pack')} onswitch={switchPack} onadd={() => show('add')} />
        {#if overview}
          <VersionEditor version={overview.version} lastTag={overview.lastTag} lastVersion={overview.lastVersion} onchange={refresh} />
          <div class="game">Minecraft {overview.minecraft} with {loaderName(overview.loader)}</div>
        {/if}
      </div>

      {#if overview}
        <dl class="facts">
          <div>
            <dt>Branch</dt>
            <dd>
              <span class="file">{overview.branch}</span>
              {#if overview.ahead}<span class="pending">{overview.ahead} to push</span>{/if}
              {#if overview.behind}<span class="pending">{overview.behind} to pull</span>{/if}
              {#if incomingSummary}<span class="pending">{incomingSummary}</span>{/if}
            </dd>
          </div>
          <div class="clickable" class:open={addonsOpen}>
            <dt>
              <button class="expand" aria-expanded={addonsOpen} aria-controls="addon-breakdown" onclick={() => (addonsOpen = !addonsOpen)}>
                Addons
                <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M4 2.5 7.5 6 4 9.5" /></svg>
              </button>
            </dt>
            <dd>{overview.lockCount}</dd>
            {#if addonsOpen}
              <dd class="breakdown" id="addon-breakdown">
                <ul>
                  {#each addonRows as [label, count] (label)}
                    <li><span>{label}</span><span>{count}</span></li>
                  {/each}
                </ul>
              </dd>
            {/if}
          </div>
        </dl>
      {/if}

      {#if packs?.active}
        <nav class="pack-nav" aria-label="Pack">
          {#each groups as group, index (index)}
            <ul aria-label={group.label}>
              {#if group.label}<li class="group" aria-hidden="true">{group.label}</li>{/if}
              {#each group.items as item (item.id)}
                {@const side = sideOf(item.id)}
                <li>
                  <button
                    class="item tone-{item.tone ?? 'idle'}"
                    class:current={view === item.id}
                    class:beside={side && view !== item.id}
                    aria-current={view === item.id ? 'page' : undefined}
                    aria-keyshortcuts={item.key ? `Control+${item.key} Control+Shift+${item.key}` : undefined}
                    title={railTitle(item.status)}
                    onclick={(e) => onRailClick(e, item.id)}
                    onauxclick={(e) => e.button === 1 && openBeside(item.id)}
                    oncontextmenu={(e) => onRailMenu(e, item.id, item.key)}
                    onpointerdown={(e) => onRailPointerDown(e, item.id)}
                    onlostpointercapture={cancelRailDrag}
                  >
                    <span class="dot" aria-hidden="true"></span>
                    <span class="label">{item.title}{#if item.status}<span class="visually-hidden">, {item.status}</span>{/if}{#if side}<span class="visually-hidden">, {side} pane</span>{/if}</span>
                    {#if item.key}<kbd aria-hidden="true">{shortcut(`Ctrl+${item.key}`)}</kbd>{/if}
                    {#if side}{@render sideMark(side)}{/if}
                    {#if item.count}
                      <span class="pending count" aria-hidden="true">{item.count}</span>
                      <span class="visually-hidden">, {item.countText}</span>
                    {/if}
                  </button>
                </li>
              {/each}
            </ul>
          {/each}
        </nav>
      {/if}
    </section>

    {#if view === 'add' && packs?.packs.length}
      <div class="item current adding" aria-current="page"><span class="label">Adding a pack</span></div>
    {/if}

    <button
      class="item app-settings"
      aria-describedby={[updateWaiting(selfUpdate.view) && 'rail-update', account !== undefined && 'rail-player'].filter(Boolean).join(' ') || undefined}
      class:current={view === 'settings'}
      class:beside={sideOf('settings') && view !== 'settings'}
      aria-current={view === 'settings' ? 'page' : undefined}
      aria-keyshortcuts="Control+,"
      title={railTitle()}
      onclick={(e) => onRailClick(e, 'settings')}
      onauxclick={(e) => e.button === 1 && packs?.active && openBeside('settings')}
      oncontextmenu={(e) => onRailMenu(e, 'settings')}
      onpointerdown={(e) => onRailPointerDown(e, 'settings')}
      onlostpointercapture={cancelRailDrag}>
      <span class="label">App settings{#if sideOf('settings')}<span class="visually-hidden">, {sideOf('settings')} pane</span>{/if}</span>
      {#if updateWaiting(selfUpdate.view)}
        <span class="chip ok" id="rail-update" title="packraven {selfUpdate.view.status.version} is {selfUpdate.view.status.phase}">
          update<span class="visually-hidden">{` to packraven ${selfUpdate.view.status.version} ${selfUpdate.view.status.phase}`}</span>
        </span>
      {/if}
      {#if account !== undefined}
        <!-- The rail has no room for both, so the update chip takes the name's place until you update. -->
        <span class="who" class:signed-out={!account} class:visually-hidden={updateWaiting(selfUpdate.view)} id="rail-player">
          <span class="visually-hidden">{'Minecraft: '}</span>{account ? account.name : 'Signed out'}
        </span>
      {/if}
      {#if sideOf('settings')}{@render sideMark(sideOf('settings')!)}{/if}
      <kbd aria-hidden="true">{shortcut('Ctrl+,')}</kbd>
    </button>
  </aside>

  <main>
    {#if split}<div class="banners">{@render banners()}</div>{/if}
    {#if tabbed}
      <div class="pane-tabs">
        <div class="tab-list" role="tablist" aria-label="Panes">
          {#each layout.panes as pane, index (pane)}
            <button
              class="pane-tab"
              role="tab"
              id="pane-{index}-tab"
              aria-selected={index === layout.focused}
              aria-controls="pane-{index}"
              tabindex={index === layout.focused ? 0 : -1}
              title="The window is too narrow for two panes side by side. F6 switches between them."
              onclick={() => (layout.focused = index)}
              onkeydown={onTabKey}
              oncontextmenu={(e) => onPaneMenu(e, index)}
            >{titles[pane]}</button>
          {/each}
        </div>
        <button class="close quiet" aria-label="Close {titles[view]}" title="Close {titles[view]}" onclick={() => closeAndFocus(layout.focused)}>
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
        </button>
      </div>
    {/if}
    <div class="panes" class:split={paneMode === 'split'} bind:this={panesEl} bind:clientWidth={panesWidth} style:grid-template-columns={paneMode === 'split' ? `${leftWidth}px ${PANE_GAP}px minmax(0, 1fr)` : null}>
      {#each layout.panes as pane, index (pane)}
        <Pane
          view={pane}
          id="pane-{index}"
          mode={paneMode}
          side={split ? (index ? 'right' : 'left') : null}
          focused={index === layout.focused}
          onfocus={() => (layout.focused = index)}
          onclose={() => closeAndFocus(index)}
          onmenu={(e) => onPaneMenu(e, index)}
          top={split ? undefined : banners}
        >
          {@render content(pane, index)}
        </Pane>
        <!-- The divider sits between the panes in the DOM too, so Tab reaches it where you see it. -->
        {#if index === 0 && paneMode === 'split'}
          <Splitter value={leftWidth} min={PANE_MIN} max={splitTotal - PANE_MIN} total={splitTotal} label="Resize the panes" controls="pane-0" onresize={(px) => paneSplit.set(px / splitTotal)} onreset={() => paneSplit.reset()} />
        {/if}
      {/each}
      {#if dropSide !== null}
        <div
          class="pane-drop {dropSide === 0 ? 'left' : 'right'}"
          style:width={paneMode === 'split' ? `${dropSide === 0 ? leftWidth : splitTotal - leftWidth}px` : '50%'}
          aria-hidden="true"
        >
          <span>{dropLabel(dropSide)}</span>
        </div>
      {/if}
    </div>
  </main>
</div>

{#snippet sideMark(side: 'left' | 'right')}
  <svg class="side" viewBox="0 0 14 10" aria-hidden="true"><rect x="0.5" y="0.5" width="13" height="9" rx="1.5" /><rect class="fill" x={side === 'left' ? 2 : 7} y="2" width="5" height="6" rx="0.5" /></svg>
{/snippet}

{#snippet banners()}
  <GitNotice oninstalled={refresh} />
  {#if view !== 'add'}<DefenderNotice />{/if}
  {#if overview && overview.hooks !== 'current' && view !== 'add' && !hooksDismissed}
    <div class="banner">
      <Notice tone="warn">
        <div class="hook-row">
          <p>
            {overview.hooks === 'missing'
              ? "This pack has no sync hooks, the git scripts that download mods when you pull, so pulling won't download them."
              : "This pack's sync hooks, the git scripts that download mods when you pull, are older than this version of packraven."}
          </p>
          <span class="hook-actions">
            <button onclick={updateHooks}>Update hooks</button>
            <button class="quiet" onclick={() => (hooksDismissed = true)}>Not now</button>
          </span>
        </div>
        {#if hookMessage}<p class:muted={!hookFailed} class:failed={hookFailed}>{hookMessage}</p>{/if}
      </Notice>
    </div>
  {/if}
{/snippet}

{#snippet loadNotice(index: number)}
  <Notice tone="error">
    <p>{loadError}</p>
    <p><button onclick={() => showFrom('settings', index)}>Open App settings</button></p>
  </Notice>
{/snippet}

{#snippet content(pane: View, index: number)}
  <!-- Setup activates the new pack before it reports done, so we keep Add pack outside the key
       block. Otherwise the switch would remount it and drop the setup result. -->
  {#if pane === 'add'}
    <AddPack first={!packs?.packs.length} onadded={() => { only('mods'); refresh() }} />
  {:else}
  {#if pane === 'settings'}
    <SettingsView onchange={refresh} />
  {:else if packs?.active}
  {#key packs.active}
    {#if loadError && !OPEN_WITHOUT_PACK.includes(pane)}
      {@render loadNotice(index)}
    {:else if pane === 'changes'}
      <Changes {overview} onchange={refresh} />
    {:else if pane === 'mods'}
      <Mods pack={packs?.active ?? null} {publishesTo} game={overview ? `${overview.minecraft} ${loaderName(overview.loader).split(' ')[0]}` : 'this pack'} clientRunning={!!overview?.clientRunning} ontest={() => showFrom('test', index)} />
    {:else if pane === 'server'}
      <Server {overview} />
    {:else if pane === 'test'}
      <TestRun {overview} onserver={() => showFrom('server', index)} onappsettings={() => showFrom('settings', index)} />
    {:else if pane === 'files'}
      {#if loadError}<div class="banner">{@render loadNotice(index)}</div>{/if}
      {#await loadFiles() then { default: Files }}
        <Files pack={packs?.active ?? null} />
      {/await}
    {:else if pane === 'git'}
      {#if loadError}<div class="banner">{@render loadNotice(index)}</div>{/if}
      <Git onchange={refresh} />
    {:else if pane === 'publish'}
      <Publish {overview} onchange={refresh} onchanges={() => showFrom('changes', index)} onsettings={() => showFrom('pack', index)} onreleases={() => showFrom('releases', index)} onrun={() => openBesideIfRoom('test', index)} onappsettings={() => showFrom('settings', index)} />
    {:else if pane === 'releases'}
      <Releases onsettings={() => showFrom('pack', index)} onappsettings={() => showFrom('settings', index)} />
    {:else if pane === 'pack' && packs?.active}
      <PackSettings dir={packs.active} name={overview?.name ?? 'this pack'} onserver={() => showFrom('server', index)} onappsettings={() => showFrom('settings', index)} onchange={refresh} onremoved={() => { only('mods'); refresh() }} />
    {:else}
      <SettingsView onchange={refresh} />
    {/if}
  {/key}
  {/if}
  {/if}
{/snippet}

{#if railGhost}
  <div class="rail-ghost" style:left="{railGhost.x + 14}px" style:top="{railGhost.y + 10}px" aria-hidden="true">
    {titles[railGhost.id]}
  </div>
{/if}

{#if railMenu}
  <ContextMenu x={railMenu.x} y={railMenu.y} label="Pane actions" items={railMenu.items} scroller={railMenu.scroller} onclose={() => (railMenu = null)} />
{/if}

<IgnoreDialog />
<UpdateChangelogDialog />

<style>
  .shell {
    display: grid;
    grid-template-columns: 264px minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr);
    height: 100vh;
  }

  .rail {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-height: 0;
    overflow-y: auto;
    padding: 20px 16px 20px 22px;
    color: var(--rail-text);
    background: var(--rail);
    border-right: 1px solid var(--rail-edge);
    /* We always reserve the 6px gutter and take it out of the right padding, so nothing shifts when
       an editor or list makes the rail scrollable. Chromium ignores ::-webkit-scrollbar while
       scrollbar-color is set, and the root sets it, so we reset it here to get a bar without arrows. */
    scrollbar-color: auto;
    scrollbar-gutter: stable;
  }

  .rail::-webkit-scrollbar {
    width: 6px;
  }

  .rail::-webkit-scrollbar-thumb {
    background: var(--rail-edge);
    border-radius: 3px;
  }

  .rail::-webkit-scrollbar-track {
    background: transparent;
  }

  .rail::-webkit-scrollbar-button {
    display: none;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 800;
    font-size: var(--step--1);
    letter-spacing: 0.02em;
    color: var(--rail-muted);
  }

  .pack {
    display: grid;
    gap: 16px;
    margin-inline: -10px;
    padding: 14px 10px 8px;
    background: var(--rail-pack);
    border: 1px solid var(--rail-edge);
    border-radius: var(--radius-panel);
  }

  .identity {
    display: grid;
    gap: 6px;
    min-width: 0;
    padding-inline: 6px;
  }

  .game {
    color: var(--rail-muted);
    font-size: var(--step--1);
  }

  .facts {
    display: grid;
    margin: 0 6px;
    border-top: 1px solid var(--rail-edge);
  }

  .facts div {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: baseline;
    gap: 4px 12px;
    padding: 7px 0;
    border-bottom: 1px solid var(--rail-edge);
  }

  .facts dt {
    color: var(--rail-muted);
    font-size: var(--step--1);
  }

  .facts dd {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    justify-content: end;
    gap: 2px 8px;
    font-variant-numeric: tabular-nums;
  }

  .facts .clickable {
    position: relative;
    isolation: isolate;
  }

  .facts .clickable::before {
    content: '';
    position: absolute;
    inset: 2px -6px;
    z-index: -1;
    border-radius: var(--radius-control);
  }

  .facts .clickable:hover::before {
    background: var(--rail-hi);
  }

  .expand {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 0;
    font: inherit;
    color: inherit;
    background: transparent;
    border: 0;
  }

  .expand::after {
    content: '';
    position: absolute;
    inset: 0;
  }

  .expand:hover:not(:disabled) {
    background: transparent;
  }

  .expand:focus-visible {
    outline: none;
  }

  .clickable:has(.expand:focus-visible) {
    outline: 2px solid var(--rail-strong);
    outline-offset: -2px;
  }

  .expand svg {
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.6;
    stroke-linecap: round;
    stroke-linejoin: round;
    transition: transform 0.15s ease;
  }

  .open .expand svg {
    transform: rotate(90deg);
  }

  .facts .breakdown {
    flex-basis: 100%;
    display: block;
    margin: 4px 0 0;
  }

  .breakdown ul {
    display: grid;
    gap: 2px;
    margin: 0;
    padding: 0 0 0 18px;
    list-style: none;
  }

  .breakdown li {
    display: flex;
    justify-content: space-between;
    color: var(--rail-muted);
    font-size: var(--step--1);
  }

  .pending {
    color: var(--rail-warn);
  }

  .pack-nav {
    display: grid;
    gap: 10px;
  }

  .pack-nav ul {
    display: grid;
    gap: 1px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .pack-nav ul + ul {
    padding-top: 10px;
    border-top: 1px solid var(--rail-edge);
  }

  .group {
    padding: 0 6px 4px;
    font-size: var(--step--2);
    font-weight: 700;
    color: var(--rail-muted);
  }

  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    padding: 4px 6px;
    font-weight: 400;
    text-align: left;
    color: var(--rail-text);
    background: transparent;
    border-color: transparent;
  }

  .item:is(:hover, :focus-visible):not(:disabled) {
    background: var(--rail-hi);
  }

  .item.current,
  .item.current:is(:hover, :focus-visible):not(:disabled) {
    font-weight: 700;
    background: var(--rail-current);
  }

  .item:focus-visible {
    outline-offset: -2px;
  }

  .item .label {
    flex: 1;
    min-width: 0;
  }

  /* We overlay the hint instead of giving it room, so a long label like Pack settings doesn't wrap
     when it appears. */
  .item {
    position: relative;
  }

  .item kbd {
    display: none;
    position: absolute;
    right: 6px;
    border-color: var(--rail-edge);
    color: var(--rail-muted);
    background: inherit;
  }

  .item:hover kbd,
  .item:focus-visible kbd {
    display: inline;
  }

  .item:has(kbd):is(:hover, :focus-visible) :is(.count, .side, .who) {
    visibility: hidden;
  }

  .item .count {
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .dot {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }

  .tone-ok .dot {
    background: var(--rail-ok);
  }

  .tone-warn .dot {
    background: var(--rail-warn);
  }

  .tone-bad .dot {
    background: var(--rail-fail);
  }

  /* The tone goes in a prefixed class, since app.css draws a spinner on any button.busy. */
  .tone-busy .dot {
    box-shadow: inset 0 0 0 2px var(--rail-muted);
    animation: rail-pulse 1.2s ease-in-out infinite;
  }

  @keyframes rail-pulse {
    50% {
      background: var(--rail-muted);
    }
  }

  .app-settings {
    margin-top: auto;
    color: var(--rail-muted);
  }

  .app-settings .chip {
    margin-left: auto;
  }


  /* We show who you play as in the App settings row, so it costs the rail no height; the shortcut hint covers it on hover. */
  .app-settings .who {
    margin-left: auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--rail-text);
    font-size: var(--step--1);
    font-weight: 600;
  }

  .app-settings .who.signed-out {
    color: var(--rail-muted);
    font-weight: 400;
  }

  .adding {
    cursor: default;
  }

  main {
    --view-inset: 32px;
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
  }

  .banner {
    margin-bottom: 18px;
  }

  .hook-row {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: center;
    gap: 6px 16px;
  }

  .hook-actions {
    display: flex;
    gap: 4px;
  }

  .hook-actions button {
    padding-block: 4px;
  }

  .rail :global(:focus-visible) {
    outline-color: var(--rail-text);
  }

  .panes {
    position: relative;
    flex: 1;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr);
    min-height: 0;
  }

  .dragging-rail,
  .dragging-rail .item {
    cursor: grabbing;
    user-select: none;
  }

  .item {
    touch-action: pan-y;
  }

  .pane-drop {
    position: absolute;
    top: 0;
    bottom: 0;
    z-index: 10;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--here) 55%, transparent);
    box-shadow: inset 0 0 0 2px var(--forest);
    pointer-events: none;
  }

  .pane-drop.left {
    left: 0;
  }

  .pane-drop.right {
    right: 0;
  }

  .pane-drop span {
    padding: 4px 12px;
    font-size: var(--step--1);
    font-weight: 700;
    white-space: nowrap;
    color: var(--ochre-ink);
    background: var(--here);
    border: 1px solid var(--here-border);
    border-radius: var(--radius-control);
    box-shadow: 0 6px 16px var(--shadow);
  }

  .rail-ghost {
    position: fixed;
    z-index: 30;
    padding: 2px 8px;
    font-size: var(--step--1);
    font-weight: 700;
    white-space: nowrap;
    color: var(--ochre-ink);
    background: var(--here);
    border: 1px solid var(--here-border);
    border-radius: var(--radius-control);
    box-shadow: 0 6px 16px var(--shadow);
    pointer-events: none;
  }

  .panes.split {
    --view-inset: 20px;
  }

  /* The panes sit 1px apart so their bars meet the divider, and the handle reaches a few pixels
     into each pane to stay easy to grab. */
  .panes :global(.splitter) {
    z-index: 1;
    margin-inline: -3px;
  }

  .panes :global(.splitter:not(.dragging, :focus-visible))::before {
    background: var(--line-strong);
  }

  .panes :global(.splitter:hover:not(.dragging, :focus-visible))::before {
    background: var(--edge);
  }

  .pane-tabs {
    display: flex;
    align-items: end;
    gap: 8px;
    padding: 6px 6px 0 var(--view-inset);
    border-bottom: 1px solid var(--line);
  }

  .tab-list {
    flex: 1;
    display: flex;
    gap: 2px;
    min-width: 0;
  }

  .pane-tab {
    min-width: 0;
    margin-bottom: -1px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 5px 14px;
    font-size: var(--step--1);
    color: var(--muted);
    background: transparent;
    border-color: transparent;
    border-bottom: 0;
    border-radius: var(--radius-control) var(--radius-control) 0 0;
  }

  .pane-tab:hover:not([aria-selected='true'], :disabled) {
    color: var(--text);
    background: var(--control);
  }

  .pane-tab[aria-selected='true'] {
    color: var(--ochre-ink);
    background: var(--here);
    border-color: var(--here-border);
  }

  .pane-tabs .close {
    display: grid;
    place-items: center;
    align-self: center;
    margin-bottom: 2px;
    padding: 4px;
  }

  .pane-tabs svg {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.4;
    stroke-linecap: round;
  }

  .banners {
    padding: 16px var(--view-inset) 0;
    border-bottom: 1px solid var(--line);
  }

  .banners:not(:has(*)) {
    display: none;
  }

  .item.beside {
    box-shadow: inset 0 0 0 1px var(--rail-edge);
  }

  .side {
    flex: none;
    width: 14px;
    height: 10px;
    fill: none;
    stroke: var(--rail-muted);
  }

  .side .fill {
    fill: var(--rail-muted);
    stroke: none;
  }

  .current .side {
    stroke: var(--rail-strong);
  }

  .current .side .fill {
    fill: var(--rail-strong);
  }

  /* On short windows we tighten the rhythm but keep the Work and Release labels. */
  @media (max-height: 720px) {
    .game {
      display: none;
    }

    .rail {
      gap: 10px;
      padding-block: 12px;
    }

    .pack {
      gap: 8px;
      padding-block: 10px 6px;
    }

    .facts div {
      padding-block: 4px;
    }

    .pack-nav {
      gap: 6px;
    }

    .pack-nav ul + ul {
      padding-top: 6px;
    }

    .group {
      padding-bottom: 1px;
    }

    .item {
      padding-block: 2px;
    }
  }

  @media (max-width: 1100px) {
    .shell {
      grid-template-columns: 220px minmax(0, 1fr);
    }

    .rail {
      padding-inline: 16px;
    }

    .pack {
      margin-inline: -6px;
    }

    main {
      --view-inset: 24px;
    }
  }
</style>

