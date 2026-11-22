<script lang="ts">
  import { onMount, tick, untrack } from 'svelte'
  import { SvelteSet } from 'svelte/reactivity'
  import { revealItemInDir } from '@tauri-apps/plugin-opener'
  import { ask } from '@tauri-apps/plugin-dialog'
  import { api, errorText, type FileChange, type GitDiff, type LineAction, type ModConflict, type Prefer, type UntrackPreview } from '../lib/api'
  import { gitCache, gitDiffCache } from '../lib/gitCache.svelte'
  import { job, onJobEnd } from '../lib/jobs.svelte'
  import { nav, showInFiles } from '../lib/nav.svelte'
  import { confirmDiscard, discardChanges, ignoreItems, stopTracking, untrackItems, untrackLabel, untrackedNotice, type Feedback } from '../lib/gitActions'
  import { LOCKFILE } from '../lib/modsync'
  import { viewerFor } from '../lib/viewers'
  import { openIgnoreDialog } from '../lib/ignoreDialog.svelte'
  import { changeRows, type ChangeRow } from '../lib/changeTree'
  import { RangeSelection, isPlainClick } from '../lib/rangeSelection'
  import { commandKey, fileManager, isMac, shortcut } from '../lib/platform'
  import type { MenuItem } from '../lib/menu'
  import ContextMenu from '../components/ContextMenu.svelte'
  import Twisty from '../components/Twisty.svelte'
  import Notice from '../components/Notice.svelte'
  import LogView from '../components/LogView.svelte'
  import LinkedText from '../components/LinkedText.svelte'
  import Splitter from '../components/Splitter.svelte'
  import DiffView from '../components/DiffView.svelte'
  import ImageDiff from '../components/ImageDiff.svelte'
  import UntrackIgnoredDialog, { type UntrackResult } from '../components/UntrackIgnoredDialog.svelte'
  import { StoredSplit, clamp } from '../lib/storedSplit.svelte'
  import { BACK_PRIORITY, useBack } from '../lib/back.svelte'
  import { paneFocus } from '../lib/panes.svelte'

  let { pack, onchange }: { pack: string | null; onchange: () => void } = $props()

  type Group = 'staged' | 'unstaged'
  type Layout = 'list' | 'tree'
  type Selection = { path: string; staged: boolean; untracked: boolean; status: string | null; origPath: string | null }

  const LAYOUT_KEY = 'packraven.gitLayout'

  untrack(() => {
    if (gitCache.pack === pack) return
    gitCache.pack = pack
    gitCache.status = null
    gitCache.commits = []
    gitDiffCache.clear()
  })

  let status = $derived(gitCache.status)
  let commits = $derived(gitCache.commits)
  // Selection keys are the group and path, with a trailing slash for folder rows in the tree.
  const keys = new SvelteSet<string>()
  const picker = new RangeSelection(keys)
  const inFocusedPane = paneFocus()

  useBack(BACK_PRIORITY.selection, () => {
    if (!inFocusedPane() || keys.size <= 1) return false
    picker.only(focused ?? [...keys][0])
    return true
  })
  const collapsed = new SvelteSet<string>()
  let focused = $state<string | null>(null)
  let layout = $state<Layout>(storedLayout())
  let diff = $state<GitDiff | null>(null)
  let diffLoading = $state(false)
  let message = $state('')
  let error = $state<string | null>(null)
  let loadError = $state<string | null>(null)
  let lastAction = $state<string | null>(null)
  let notice = $state<string | null>(null)
  let menu = $state<{ x: number; y: number; items: MenuItem[]; scroller: EventTarget | null } | null>(null)
  let conflicts = $state<ModConflict[] | null>(null)
  let checkingPull = $state(false)
  let reviewingIgnored = $state<string[] | null>(null)
  let untracked = $state<string[] | null>(null)
  let untrackedNoticeEl = $state<HTMLElement>()
  let changesHeading = $state<HTMLElement>()
  let summaryUntrack = $state<UntrackPreview | null>(null)

  const statusNames: Record<string, string> = {
    M: 'Modified',
    A: 'Added',
    D: 'Deleted',
    R: 'Renamed',
    C: 'Copied',
    T: 'Type changed',
    '?': 'New',
    U: 'Conflict',
  }

  // The poll clears only its own error, so a failed stage or commit stays on screen until your next action.
  // Each reload gets a sequence number so a slower poll never overwrites a fresher post-action status.
  let loadSeq = 0
  let appliedLoadSeq = 0
  let activeLoads = 0

  async function load() {
    const seq = ++loadSeq
    activeLoads++
    try {
      const result = await api.gitOverview()
      if (seq < appliedLoadSeq || gitCache.pack !== pack) return
      appliedLoadSeq = seq
      gitCache.status = result.status
      gitCache.commits = result.commits
      loadError = null
      for (const key of [...keys]) if (!changesFor(key).length) keys.delete(key)
    } catch (e) {
      if (seq >= appliedLoadSeq && gitCache.pack === pack) loadError = errorText(e)
    } finally {
      activeLoads--
    }
  }

  // Each tick we also re-read the open diff, so edits on disk show up without you reselecting the file.
  function poll() {
    if (document.hidden || activeLoads > 0) return
    load().then(() => selected && loadDiff(selected, false))
  }

  const SIDE_GAP = 24
  const SIDE_MIN = 260
  const DIFF_MIN = 360
  const sideSplit = new StoredSplit('packraven.gitSideWidth')
  let workWidth = $state(0)
  let splitTotal = $derived(workWidth - SIDE_GAP)
  let sideMax = $derived(splitTotal - DIFF_MIN)
  let sideWidth = $derived(clamp(sideSplit.value ?? splitTotal * 0.34, SIDE_MIN, sideMax))
  // Below this, the diff beside the list would be too narrow to read, as in half of a split, so we
  // stack the diff under the list.
  let stacked = $derived(workWidth > 0 && workWidth < SIDE_MIN + SIDE_GAP + 200)

  function storedLayout(): Layout {
    try {
      return localStorage.getItem(LAYOUT_KEY) === 'tree' ? 'tree' : 'list'
    } catch {
      return 'list'
    }
  }

  // Folder rows only exist in the tree, so we drop them from the selection when you leave it.
  function setLayout(next: Layout) {
    layout = next
    if (next === 'list') for (const key of [...keys]) if (key.endsWith('/')) keys.delete(key)
    try {
      localStorage.setItem(LAYOUT_KEY, next)
    } catch {}
  }

  function keyOf(group: Group, row: { path: string; isDir: boolean }) {
    return `${group}:${row.path}${row.isDir ? '/' : ''}`
  }

  function parseKey(key: string) {
    const at = key.indexOf(':')
    const rest = key.slice(at + 1)
    const isDir = rest.endsWith('/')
    return { group: key.slice(0, at) as Group, path: isDir ? rest.slice(0, -1) : rest, isDir }
  }

  function changesFor(key: string): FileChange[] {
    const { group, path, isDir } = parseKey(key)
    return groupChanges[group].filter((c) => (isDir ? c.path.startsWith(`${path}/`) : c.path === path))
  }

  function selectedChanges(group: Group) {
    const found = new Map<string, FileChange>()
    for (const key of keys) if (key.startsWith(`${group}:`)) for (const change of changesFor(key)) found.set(change.path, change)
    return [...found.values()]
  }

  function selectedPaths() {
    return [...new Set([...keys].map((key) => parseKey(key).path))]
  }

  onMount(() => {
    load()
    const timer = setInterval(poll, 5000)
    return () => clearInterval(timer)
  })

  onJobEnd('git', (phase) => {
    load()
    onchange()
    if (phase === 'done' && lastAction === 'commit') message = ''
  })

  // We clear the old diff on a new selection, so the header never sits over another file's changes.
  // A poll that finds the same diff leaves it alone, so the lines you picked stay picked.
  let diffSeq = 0
  let appliedDiffSeq = 0
  // A diff from the cache may be stale, so we only apply picked lines once git confirms it.
  let diffConfirmed = $state(false)

  function diffCacheKey(current: Selection) {
    return `${current.staged ? 's' : 'u'}:${current.path}`
  }

  function applyDiff(current: Selection, next: GitDiff, seq: number) {
    if (seq < appliedDiffSeq || selected !== current) return
    appliedDiffSeq = seq
    if (gitCache.pack === pack) gitDiffCache.set(diffCacheKey(current), next)
    if (next.text !== diff?.text || next.binary !== diff.binary || next.truncated !== diff.truncated) diff = next
    diffConfirmed = true
  }

  function loadDiff(current: Selection, fresh: boolean) {
    const seq = ++diffSeq
    if (fresh) {
      diffConfirmed = false
      const cached = gitCache.pack === pack ? gitDiffCache.get(diffCacheKey(current)) : undefined
      if (cached) {
        diff = cached
        diffLoading = false
      } else {
        diff = null
        diffLoading = true
      }
    }
    return api
      .gitDiff(current.path, current.staged, current.untracked)
      .then((d) => applyDiff(current, d, seq))
      .catch((e) => seq >= appliedDiffSeq && selected === current && (error = errorText(e)))
      .finally(() => seq >= appliedDiffSeq && selected === current && (diffLoading = false))
  }

  $effect(() => {
    const current = selected
    if (!current) {
      diff = null
      return
    }
    loadDiff(current, true)
  })

  // The Files tree's "Show diff" sends you here with a path to select.
  $effect(() => {
    const path = nav.diffPath
    if (!path || !status) return
    nav.diffPath = null
    const group: Group | null = status.unstaged.some((c) => c.path === path) ? 'unstaged' : status.staged.some((c) => c.path === path) ? 'staged' : null
    if (!group) return
    for (let at = path.indexOf('/'); at > 0; at = path.indexOf('/', at + 1)) collapsed.delete(`${group}:${path.slice(0, at)}`)
    const key = keyOf(group, { path, isDir: false })
    picker.only(key)
    focused = key
    tick().then(() => rowElement(key)?.scrollIntoView({ block: 'nearest' }))
  })

  async function run(action: () => Promise<void>) {
    try {
      error = null
      notice = null
      untracked = null
      await action()
      await load()
      onchange()
    } catch (e) {
      error = errorText(e)
    }
  }

  async function discard(changes: FileChange[]) {
    if (changes.length && (await confirmDiscard(changes))) await run(() => discardChanges(changes))
  }

  function stage(changes: FileChange[]) {
    if (changes.length) run(() => api.gitStage(changes.map((c) => c.path)))
  }

  function unstage(changes: FileChange[]) {
    if (changes.length) run(() => api.gitUnstage(changes.map((c) => c.path)))
  }

  const feedback: Feedback = {
    ondone: () => (load(), onchange()),
    onerror: (text) => (error = text),
    onnotice: (text) => (notice = text),
    onuntracked: showUntracked,
  }

  // Untracking acts on the changed files you picked, not every tracked file under a folder row.
  function selectedChangePaths() {
    return [...new Set([...selectedStaged, ...selectedUnstaged].map((c) => c.path))]
  }

  // The control you used to get here may be gone after untracking, so we move focus to the result.
  function showUntracked(paths: string[]) {
    notice = null
    untracked = paths
    tick().then(() => untrackedNoticeEl?.focus())
  }

  function undoUntrack() {
    const paths = untracked
    if (paths) run(() => api.gitUnstage(paths))
  }

  function untrackedIgnored({ untracked: paths, stillIgnored }: UntrackResult) {
    reviewingIgnored = null
    if (paths.length) showUntracked(paths)
    else {
      untracked = null
      tick().then(() => changesHeading?.focus())
    }
    if (stillIgnored.length) {
      const one = stillIgnored.length === 1
      notice = `Git can't keep ${one ? stillIgnored[0] : `${stillIgnored.length} files`} tracked with an exception, because your .gitignore leaves out ${one ? 'its folder' : 'their folders'}. In .gitignore, leave out the folder's contents instead, like config/spark/* rather than config/spark/.`
    }
    load()
    onchange()
  }

  async function ignoreSelection() {
    const paths = selectedPaths()
    try {
      openIgnoreDialog({ paths, options: await api.gitIgnoreOptions(paths), chosen: 0, feedback })
    } catch (e) {
      error = errorText(e)
    }
  }

  async function attempt(action: () => Promise<void>) {
    try {
      error = null
      await action()
    } catch (e) {
      error = errorText(e)
    }
  }

  function rowElement(key: string) {
    return document.querySelector<HTMLElement>(`.changes [data-key="${CSS.escape(key)}"]`)
  }

  async function focusKey(key: string) {
    focused = key
    await tick()
    rowElement(key)?.focus()
  }

  function toggleFolder(group: Group, path: string, open = collapsed.has(`${group}:${path}`)) {
    if (open) collapsed.delete(`${group}:${path}`)
    else collapsed.add(`${group}:${path}`)
  }

  function onRowClick(event: MouseEvent, group: Group, row: ChangeRow) {
    const key = keyOf(group, row)
    focused = key
    picker.click(event, key, order[group])
    if (isPlainClick(event) && row.isDir) toggleFolder(group, row.path)
  }

  function onRowDoubleClick(event: MouseEvent, group: Group, row: ChangeRow) {
    if (row.isDir || !isPlainClick(event)) return
    const changes = changesFor(keyOf(group, row))
    if (group === 'staged') unstage(changes)
    else stage(changes)
  }

  // We add the focused row to the selection first, so keys act on the row you can see is focused.
  function onGroupKey(event: KeyboardEvent, group: Group) {
    const list = order[group]
    const index = list.indexOf(focused ?? '')
    const row = rowsByGroup[group][index]
    if (!row) return
    const key = list[index]
    const move = (to: number) => {
      const target = list[Math.max(0, Math.min(to, list.length - 1))]
      if (event.shiftKey) picker.range(list, target)
      else picker.only(target)
      focusKey(target)
    }
    const ensureSelected = () => keys.has(key) || picker.only(key)
    const folderOpen = row.isDir && !collapsed.has(`${group}:${row.path}`)
    if (commandKey(event) && event.key.toLowerCase() === 'a') {
      picker.replace(list)
    } else if (event.key === 'ArrowDown') {
      move(index + 1)
    } else if (event.key === 'ArrowUp') {
      move(index - 1)
    } else if (event.key === 'Home') {
      move(0)
    } else if (event.key === 'End') {
      move(list.length - 1)
    } else if (event.key === 'ArrowRight' && row.isDir) {
      if (folderOpen) move(index + 1)
      else toggleFolder(group, row.path, true)
    } else if (event.key === 'ArrowLeft' && folderOpen) {
      toggleFolder(group, row.path, false)
    } else if (event.key === 'ArrowLeft' && row.depth > 0) {
      move(rowsByGroup[group].findLastIndex((r, i) => i < index && r.depth === row.depth - 1))
    } else if (event.key === ' ') {
      if (commandKey(event)) picker.toggle(key)
      else if (event.shiftKey) picker.range(list, key)
      else picker.only(key)
    } else if (event.key === 'Enter') {
      ensureSelected()
      if (group === 'staged') unstage(selectedStaged)
      else stage(selectedUnstaged)
    } else if (event.key === 'Delete' || (isMac && event.metaKey && event.key === 'Backspace')) {
      ensureSelected()
      discard(selectedUnstaged)
    } else if (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) {
      ensureSelected()
      const rect = (event.target as HTMLElement).getBoundingClientRect()
      openSelectionMenu(rect.left + 24, rect.bottom, event.target)
    } else {
      return
    }
    event.preventDefault()
  }

  function onRowMenu(event: MouseEvent, group: Group, row: ChangeRow) {
    event.preventDefault()
    event.stopPropagation()
    const key = keyOf(group, row)
    if (!keys.has(key)) picker.only(key)
    focused = key
    // The keyboard menu key fires contextmenu without pointer coordinates.
    const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
    const fromKeyboard = !event.clientX && !event.clientY
    openSelectionMenu(fromKeyboard ? rect.left + 24 : event.clientX, fromKeyboard ? rect.bottom : event.clientY, event.currentTarget)
  }

  // A deleted file has nothing on disk to open or show, but you can still copy its path.
  async function openSelectionMenu(x: number, y: number, scroller: EventTarget | null) {
    const staged = selectedStaged
    const unstaged = selectedUnstaged
    const paths = selectedPaths()
    const only = keys.size === 1 ? [...keys][0] : null
    const single = only ? parseKey(only) : null
    const exists = single?.isDir || (!!only && changesFor(only)[0]?.status !== 'D')
    const mixed = staged.length > 0 && unstaged.length > 0
    const files = (n: number) => (n === 1 && !mixed ? '' : ` ${n} ${n === 1 ? 'file' : 'files'}`)
    const [ignore, untrack] = await Promise.all([ignoreItems(paths, feedback), untrackItems(selectedChangePaths(), feedback)])
    menu = {
      x,
      y,
      scroller,
      items: [
        ...(unstaged.length ? [{ label: `Stage${files(unstaged.length)}`, shortcut: 'Enter', run: () => stage(unstaged) }] : []),
        ...(staged.length ? [{ label: `Unstage${files(staged.length)}`, shortcut: unstaged.length ? undefined : 'Enter', run: () => unstage(staged) }] : []),
        'separator',
        ...(single && exists
          ? [
              { label: 'Open in Files', run: () => showInFiles(single.path) },
              ...(single.isDir ? [] : [{ label: 'Open in default app', run: () => attempt(() => api.openWithDefaultApp(single.path)) }]),
              { label: `Show in ${fileManager}`, run: () => attempt(async () => revealItemInDir(await api.absolutePath(single.path))) },
            ]
          : []),
        'separator',
        { label: paths.length > 1 ? 'Copy paths' : 'Copy path', run: () => attempt(() => api.copyPaths(paths, true)) },
        { label: paths.length > 1 ? 'Copy relative paths' : 'Copy relative path', run: () => attempt(() => api.copyPaths(paths, false)) },
        'separator',
        ...(ignore.length ? [{ label: 'Add to .gitignore', items: ignore }] : []),
        ...untrack,
        ...(unstaged.length
          ? ['separator' as const, { label: `Discard changes${unstaged.length === 1 ? '' : ` to ${unstaged.length} files`}`, shortcut: shortcut('Delete'), danger: true, run: () => discard(unstaged) }]
          : []),
      ],
    }
  }

  const DISCARD_PREVIEW_LINES = 6
  const DISCARD_PREVIEW_CHARS = 60

  function discardPreview(text: string, lines: number[]) {
    const raw = text.split('\n')
    const shown = lines.slice(0, DISCARD_PREVIEW_LINES).map((i) => {
      const line = (raw[i] ?? '').replace(/\r$/, '')
      const body = line.slice(1)
      return `${line[0] === '+' ? '+' : '−'} ${body.length > DISCARD_PREVIEW_CHARS ? `${body.slice(0, DISCARD_PREVIEW_CHARS - 1)}…` : body}`
    })
    const more = lines.length - shown.length
    return [...shown, ...(more > 0 ? [`and ${more} more`] : [])].join('\n')
  }

  // DiffView shows apply errors next to the lines, so we throw them back to it instead of
  // reporting them in this column. A false result means you cancelled.
  async function applyLines(action: LineAction, lines: number[]) {
    const current = selected
    const shown = diff
    if (!current || !shown) return false
    if (!diffConfirmed) throw 'packraven is still checking this diff against the file. Try again in a moment.'
    if (action === 'discard') {
      const count = lines.length
      const confirmed = await ask(
        `Discard ${count === 1 ? 'this line' : `these ${count} lines`} in ${current.path}?\n\n${discardPreview(shown.text, lines)}\n\npackraven deletes the added lines and puts the removed ones back. This can't be undone.`,
        { title: 'Discard lines', kind: 'warning', okLabel: 'Discard', cancelLabel: 'Cancel' },
      )
      if (!confirmed) return false
    }
    try {
      await api.gitApplyLines(current.path, shown.text, lines, action)
    } catch (e) {
      throw errorText(e)
    } finally {
      const seq = ++diffSeq
      const nextDiff = api.gitDiff(current.path, current.staged, current.untracked).then(
        (d) => ({ ok: true as const, d }),
        (e) => ({ ok: false as const, e }),
      )
      // Show remaining diff changes as soon as git diff finishes instead of waiting on status;
      // an empty diff waits for status so a fully reverted file closes without flashing "No content changes".
      nextDiff.then((res) => {
        if (res.ok && res.d.text.trim()) applyDiff(current, res.d, seq)
      })
      const [res] = await Promise.all([nextDiff, load()])
      if (res.ok) applyDiff(current, res.d, seq)
      else if (seq >= appliedDiffSeq && selected === current) error = errorText(res.e)
      onchange()
    }
    return true
  }

  async function commit() {
    if (!canCommit) return
    lastAction = 'commit'
    await run(() => api.gitCommit(message))
  }

  async function sync(action: 'fetch' | 'push') {
    lastAction = action
    await run(() => api.gitSync(action))
  }

  // We fetch and merge the mod lists first, so you choose between conflicting mods before git changes anything.
  async function pull() {
    lastAction = 'pull'
    conflicts = null
    checkingPull = true
    try {
      error = null
      const preview = await api.gitPullPreview()
      if (preview.conflicts.length) {
        conflicts = preview.conflicts
        return
      }
    } catch (e) {
      error = `Couldn't check the remote: ${errorText(e)}`
      return
    } finally {
      checkingPull = false
    }
    await run(() => api.gitSync('pull'))
  }

  async function pullPreferring(prefer: Prefer) {
    conflicts = null
    await run(() => api.gitSync('pull', prefer))
  }

  function split(path: string) {
    const index = path.lastIndexOf('/')
    return index < 0 ? { dir: '', name: path } : { dir: path.slice(0, index + 1), name: path.slice(index + 1) }
  }

  let busy = $derived(job('git').phase === 'running' || checkingPull)
  let canCommit = $derived(!busy && !!status?.staged.length && message.trim().length > 0)
  let commitHint = $derived(
    !status?.staged.length
      ? status?.unstaged.length
        ? 'Stage something, then you can commit it.'
        : 'Nothing to commit.'
      : !message.trim()
        ? 'Write a message describing the change.'
        : null,
  )
  let groupChanges = $derived({ staged: status?.staged ?? [], unstaged: status?.unstaged ?? [] })
  let rowsByGroup = $derived({
    staged: changeRows(groupChanges.staged, layout === 'tree', (dir) => collapsed.has(`staged:${dir}`)),
    unstaged: changeRows(groupChanges.unstaged, layout === 'tree', (dir) => collapsed.has(`unstaged:${dir}`)),
  })
  let order = $derived({ staged: rowsByGroup.staged.map((r) => keyOf('staged', r)), unstaged: rowsByGroup.unstaged.map((r) => keyOf('unstaged', r)) })
  let tabStop = $derived({
    staged: focused && order.staged.includes(focused) ? focused : order.staged[0],
    unstaged: focused && order.unstaged.includes(focused) ? focused : order.unstaged[0],
  })
  let selectedStaged = $derived(selectedChanges('staged'))
  let selectedUnstaged = $derived(selectedChanges('unstaged'))
  // We show a diff when you select exactly one file. Each poll replaces every change, so we keep
  // the selected one's status as text and only reload the diff when that text changes, such as
  // when staging some of a new file's lines makes git track it.
  let diffKey = $derived(keys.size === 1 && ![...keys][0].endsWith('/') ? [...keys][0] : null)
  let selectedChange = $derived.by(() => {
    if (!diffKey) return null
    const { group, path } = parseKey(diffKey)
    const change = groupChanges[group].find((c) => c.path === path)
    return change ? `${change.status}\0${change.origPath ?? ''}` : null
  })
  let selected = $derived.by((): Selection | null => {
    if (!diffKey) return null
    const { group, path } = parseKey(diffKey)
    const [status, origPath] = selectedChange?.split('\0') ?? [null, null]
    return { path, staged: group === 'staged', untracked: status === '?', status, origPath: origPath || null }
  })

  // Keyed by status too, so the label catches up once untracking changes a file's status.
  let summaryKey = $derived(!selected && keys.size ? [...selectedStaged, ...selectedUnstaged].map((c) => `${c.status} ${c.path}`).join('\n') : '')

  $effect(() => {
    const key = summaryKey
    summaryUntrack = null
    if (!key) return
    let stale = false
    api
      .gitUntrackPreview(selectedChangePaths())
      .then((preview) => !stale && (summaryUntrack = preview))
      .catch(() => {})
    return () => (stale = true)
  })
  let gitJob = $derived(job('git'))
</script>

{#snippet changeRow(group: Group, row: ChangeRow)}
  {@const key = keyOf(group, row)}
  {@const code = row.change ? (row.change.status === '?' ? 'N' : row.change.status) : null}
  {@const parts = split(row.path)}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_noninteractive_tabindex (the group handles keys, and Svelte can't read the role) -->
  <div
    role={layout === 'tree' ? 'treeitem' : 'option'}
    aria-selected={keys.has(key)}
    aria-level={layout === 'tree' ? row.depth + 1 : undefined}
    aria-expanded={row.isDir ? !collapsed.has(`${group}:${row.path}`) : undefined}
    tabindex={key === tabStop[group] ? 0 : -1}
    data-key={key}
    class="row"
    class:selected={keys.has(key)}
    style="--depth: {row.depth}"
    title={row.isDir ? undefined : `Double-click to ${group === 'staged' ? 'unstage' : 'stage'}`}
    onclick={(e) => onRowClick(e, group, row)}
    ondblclick={(e) => onRowDoubleClick(e, group, row)}
    oncontextmenu={(e) => onRowMenu(e, group, row)}
    onfocus={() => (focused = key)}
  >
    {#if layout === 'tree'}<Twisty open={row.isDir ? !collapsed.has(`${group}:${row.path}`) : null} />{/if}
    {#if row.isDir}
      <span class="path"><span class="name folder">{row.name}</span></span>
      <span class="count">{row.count}</span>
    {:else if row.change}
      <span class="badge s{code}" title={statusNames[row.change.status] ?? row.change.status}>{code}</span>
      <span class="visually-hidden">{statusNames[row.change.status] ?? row.change.status}:</span>
      <span class="path">{#if layout === 'list'}<span class="dir">{parts.dir}</span>{/if}<span class="name">{parts.name}</span></span>
    {/if}
    <!-- Hidden from screen readers because options can't hold buttons; Enter, Delete and the menu key cover them. -->
    <!-- svelte-ignore a11y_no_static_element_interactions (only stops the row's double-click) -->
    <span class="row-actions" aria-hidden="true" ondblclick={(e) => e.stopPropagation()}>
      {#if group === 'staged'}
        <button class="icon" tabindex="-1" aria-label="Unstage {row.path}" title="Unstage" onclick={(e) => (e.stopPropagation(), unstage(changesFor(key)))}>−</button>
      {:else}
        <button class="icon" tabindex="-1" aria-label="Discard changes to {row.path}" title="Discard changes" onclick={(e) => (e.stopPropagation(), discard(changesFor(key)))}>
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M5 3 2 6l3 3M2.5 6H10a4 4 0 0 1 0 8H7" /></svg>
        </button>
        <button class="icon" tabindex="-1" aria-label="Stage {row.path}" title="Stage" onclick={(e) => (e.stopPropagation(), stage(changesFor(key)))}>+</button>
      {/if}
    </span>
  </div>
{/snippet}

{#snippet changeGroup(group: Group, heading: string)}
  <div class="changes" role={layout === 'tree' ? 'tree' : 'listbox'} aria-multiselectable="true" aria-labelledby={heading} tabindex="-1" onkeydown={(e) => onGroupKey(e, group)}>
    {#each rowsByGroup[group] as row (keyOf(group, row))}{@render changeRow(group, row)}{/each}
  </div>
{/snippet}

<div class="git">
  <header class="head">
    <div>
      <h2>Git</h2>
      <p class="muted">
        On <span class="file">{status?.branch ?? 'no branch'}</span>.
        {#if !status?.hasRemote}
          No remote set, so there's nowhere to push yet.
        {:else if !status.upstream}
          Not pushed yet. Push to create the branch on the remote.
        {:else if status.ahead || status.behind}
          {[status.ahead && `${status.ahead} to push`, status.behind && `${status.behind} to pull`].filter(Boolean).join(', ')}.
        {:else}
          Up to date with {status.upstream}.
        {/if}
      </p>
    </div>
    <div class="sync">
      <button disabled={busy || !status?.hasRemote} onclick={() => sync('fetch')} class:busy={busy && lastAction === 'fetch'}>Fetch</button>
      <button disabled={busy || !status?.upstream} onclick={pull} class:busy={busy && lastAction === 'pull'} class:primary={!!status?.behind}>Pull</button>
      <button disabled={busy || !status?.hasRemote || !status?.hasCommits} onclick={() => sync('push')} class:busy={busy && lastAction === 'push'} class:primary={!!status?.ahead && !status?.behind}>Push</button>
    </div>
  </header>

  {#if conflicts}
    <Notice tone="warn">
      <div class="conflicts">
        <p>
          You and the remote both changed {conflicts.length === 1 ? 'this mod' : `these ${conflicts.length} mods`}. Choose whose versions to keep; packraven
          keeps every other change from both sides.
        </p>
        <ul class="conflict-list">
          {#each conflicts as conflict (conflict.name)}
            <li>
              <span class="file">{conflict.name}</span>
              <dl>
                <dt>Yours</dt><dd class:file={!!conflict.mine}>{conflict.mine ?? 'Removed'}</dd>
                <dt>Theirs</dt><dd class:file={!!conflict.theirs}>{conflict.theirs ?? 'Removed'}</dd>
              </dl>
            </li>
          {/each}
        </ul>
        <div class="conflict-actions">
          <button disabled={busy} onclick={() => pullPreferring('mine')}>Pull, keeping yours</button>
          <button disabled={busy} onclick={() => pullPreferring('theirs')}>Pull, keeping theirs</button>
          <button class="quiet" onclick={() => (conflicts = null)}>Cancel</button>
        </div>
      </div>
    </Notice>
  {/if}

  <div class="work" class:stacked bind:clientWidth={workWidth} style:grid-template-columns={workWidth && !stacked ? `${sideWidth}px ${SIDE_GAP}px minmax(0, 1fr)` : null}>
  <div class="side" id="git-side">

    <form class="commit" onsubmit={(e) => { e.preventDefault(); commit() }}>
      <label for="commit-message" class="visually-hidden">Commit message</label>
      <textarea
        id="commit-message"
        bind:value={message}
        rows="3"
        placeholder="Commit message"
        aria-describedby="commit-hint"
        onkeydown={(e) => (e.ctrlKey || e.metaKey) && e.key === 'Enter' && (e.preventDefault(), commit())}
      ></textarea>
      <div class="commit-row">
        <span id="commit-hint" class="muted small">{commitHint ?? `Press ${shortcut('Ctrl+Enter')} to commit.`}</span>
        <button type="submit" class="primary" class:busy={busy && lastAction === 'commit'} disabled={!canCommit}>
          {busy && lastAction === 'commit' ? 'Committing' : `Commit ${status?.staged.length ?? 0} ${status?.staged.length === 1 ? 'file' : 'files'}`}
        </button>
      </div>
    </form>

    {#if error}<Notice tone="error"><p>{error}</p></Notice>{/if}
    {#if loadError}<Notice tone="error"><p>Couldn't read the repository: {loadError}</p></Notice>{/if}
    {#if notice}<Notice tone="warn"><p>{notice}</p></Notice>{/if}
    {#if untracked}
      <div class="focus-target" tabindex="-1" bind:this={untrackedNoticeEl}>
        <Notice tone="ok"><p>{untrackedNotice(untracked)} <button class="link" onclick={undoUntrack}>Undo</button></p></Notice>
      </div>
    {/if}
    {#if gitJob.phase === 'failed'}<Notice tone="error"><p><LinkedText text={gitJob.message ?? ''} /></p></Notice>{/if}
    {#if status?.conflicted.length}
      <Notice tone="warn"><p>{status.conflicted.length === 1 ? '1 file has a merge conflict' : `${status.conflicted.length} files have merge conflicts`}. Resolve them in an editor, then stage them.</p></Notice>
    {/if}
    {#if status?.trackedIgnored.length}
      {@const ignoredCount = status.trackedIgnored.length}
      <Notice tone="warn">
        <p>
          Git still tracks {ignoredCount === 1 ? '1 file' : `${ignoredCount} files`} your .gitignore leaves out.
          <button class="link" onclick={() => (reviewingIgnored = [...status!.trackedIgnored])}>Review…</button>
        </p>
      </Notice>
    {/if}

    <div class="layout-row">
      <span class="modes" role="group" aria-label="Show changes as">
        <button class="mode" aria-pressed={layout === 'list'} onclick={() => setLayout('list')}>List</button>
        <button class="mode" aria-pressed={layout === 'tree'} onclick={() => setLayout('tree')}>Tree</button>
      </span>
    </div>

    <section class="group" aria-labelledby="staged-heading">
      <div class="group-head">
        <h3 id="staged-heading">Staged <span class="count">{status?.staged.length ?? 0}</span></h3>
        {#if status?.staged.length}
          <button class="link" onclick={() => run(() => api.gitUnstage(status!.staged.map((c) => c.path)))}>Unstage all</button>
        {/if}
      </div>
      {#if status?.staged.length}
        {@render changeGroup('staged', 'staged-heading')}
      {:else}
        <p class="muted small empty">Staged files will appear here.</p>
      {/if}
    </section>

    <section class="group" aria-labelledby="changes-heading">
      <div class="group-head">
        <h3 id="changes-heading" tabindex="-1" bind:this={changesHeading}>Changes <span class="count">{status?.unstaged.length ?? 0}</span></h3>
        {#if status?.unstaged.length}
          <button class="link" onclick={() => run(() => api.gitStage(status!.unstaged.map((c) => c.path)))}>Stage all</button>
        {/if}
      </div>
      {#if status?.unstaged.length}
        {@render changeGroup('unstaged', 'changes-heading')}
      {:else if status}
        <p class="muted small empty">No unstaged changes. Mod changes will only show here as a change to mods.lock.json.</p>
      {/if}
    </section>

    {#if gitJob.lines.length}
      <section class="group">
        <h3>Last git run</h3>
        <div class="git-log"><LogView lines={gitJob.lines} label="Git output" /></div>
      </section>
    {/if}

    <section class="group" aria-labelledby="history-heading">
      <h3 id="history-heading">Recent commits</h3>
      {#if commits.length}
        <ol class="history">
          {#each commits as c (c.hash)}
            <li>
              <span class="subject">{c.subject}</span>
              <span class="muted small"><span class="file">{c.short}</span> {c.author}, {c.when}</span>
            </li>
          {/each}
        </ol>
      {:else}
        <p class="muted small empty">No commits yet.</p>
      {/if}
    </section>
  </div>

  {#if !stacked}
    <Splitter value={sideWidth} min={SIDE_MIN} max={sideMax} label="Resize the changes list" controls="git-side" onresize={(px) => sideSplit.set(px)} onreset={() => sideSplit.reset()} />
  {/if}

  <section class="diff" aria-label="Diff">
    {#if !selected && keys.size}
      {@const total = selectedStaged.length + selectedUnstaged.length}
      <div class="diff-head">
        <span class="selection-count">{total === 1 ? '1 file selected' : `${total} files selected`}</span>
        <span class="muted small state">{[selectedStaged.length && `${selectedStaged.length} staged`, selectedUnstaged.length && `${selectedUnstaged.length} not staged`].filter(Boolean).join(', ')}</span>
      </div>
      <div class="pad summary">
        <div class="summary-actions">
          {#if selectedUnstaged.length}<button class="primary" onclick={() => stage(selectedUnstaged)}>Stage {selectedUnstaged.length === 1 ? '1 file' : `${selectedUnstaged.length} files`}</button>{/if}
          {#if selectedStaged.length}<button onclick={() => unstage(selectedStaged)}>Unstage {selectedStaged.length === 1 ? '1 file' : `${selectedStaged.length} files`}</button>{/if}
          <button onclick={ignoreSelection}>Add to .gitignore…</button>
          {#if summaryUntrack && (summaryUntrack.ignored.length || summaryUntrack.uncovered.length)}
            {@const preview = summaryUntrack}
            <button onclick={() => stopTracking(selectedChangePaths(), feedback, preview)}>{untrackLabel(preview)}</button>
          {/if}
          {#if selectedUnstaged.length}<button class="danger" onclick={() => discard(selectedUnstaged)}>Discard changes{selectedUnstaged.length === 1 ? '' : ` to ${selectedUnstaged.length} files`}</button>{/if}
        </div>
        <ul class="summary-list" aria-label="Selected files">
          {#each [...selectedStaged.map((c) => ({ c, group: 'Staged' })), ...selectedUnstaged.map((c) => ({ c, group: 'Not staged' }))] as item (`${item.group}:${item.c.path}`)}
            <li>
              <span class="badge s{item.c.status === '?' ? 'N' : item.c.status}" title={statusNames[item.c.status] ?? item.c.status}>{item.c.status === '?' ? 'N' : item.c.status}</span>
              <span class="file path-text">{item.c.path}</span>
              <span class="muted small state">{item.group}</span>
            </li>
          {/each}
        </ul>
      </div>
    {:else if !selected}
      <div class="placeholder">
        <p>Select a file to see what changed.</p>
        <p class="small">Shift or {isMac ? '⌘' : 'Ctrl'}+click to select several.</p>
      </div>
    {:else}
      <div class="diff-head">
        <span class="file path-text" title={selected.path}>{selected.path}</span>
        <span class="muted small state">{selected.staged ? 'Staged' : selected.untracked ? 'New file' : 'Not staged'}</span>
      </div>
      {#if !diff && diffLoading}
        <p class="muted pad">Loading the diff.</p>
      {:else if diff?.binary && viewerFor(selected.path) === 'image'}
        <ImageDiff path={selected.path} origPath={selected.origPath} staged={selected.staged} status={selected.status} version={diff.text} />
      {:else if diff?.binary}
        <p class="muted pad">This is a binary file, so there's no text diff to show.</p>
      {:else if diff && !diff.text.trim()}
        <p class="muted pad">No content changes. Git flagged this file over line endings or file metadata only; you can stage it to clear the flag.</p>
      {:else if diff}
        {#if diff.truncated}<div class="pad"><Notice tone="warn"><p>This diff is very large, so packraven shows only the first part, and you can only stage the whole file.</p></Notice></div>{/if}
        <DiffView text={diff.text} path={selected.path} staged={selected.staged} selectable={!diff.truncated} canDiscard={selected.path !== LOCKFILE} onapply={applyLines} />
      {/if}
    {/if}
  </section>
  </div>
</div>

<UntrackIgnoredDialog paths={reviewingIgnored} ondone={untrackedIgnored} oncancel={() => (reviewingIgnored = null)} />

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} label="Change actions" items={menu.items} scroller={menu.scroller} onclose={() => (menu = null)} />
{/if}

<style>
  .conflicts {
    display: grid;
    gap: 10px;
  }

  .conflict-list {
    display: grid;
    gap: 10px;
    margin: 0;
    padding: 0;
    list-style: none;
    overflow-wrap: anywhere;
  }

  .conflict-list dl {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    gap: 0 10px;
    margin: 2px 0 0;
    font-size: var(--step--1);
  }

  .conflict-list dt {
    color: var(--muted);
  }

  .conflict-list dd {
    margin: 0;
  }

  .focus-target {
    border-radius: var(--radius-panel);
  }

  .conflict-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .git {
    display: flex;
    flex-direction: column;
    gap: 18px;
    flex: 1;
    min-height: 420px;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: end;
    gap: 12px 24px;
  }

  .head p {
    margin-top: 4px;
  }

  .work {
    flex: 1;
    display: grid;
    grid-template-columns: minmax(260px, 34%) 24px minmax(0, 1fr);
    min-height: 0;
  }

  .work.stacked {
    grid-template-columns: minmax(0, 1fr);
    grid-template-rows: minmax(0, 1fr) minmax(0, 1fr);
    gap: 16px;
  }

  .side {
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-height: 0;
    overflow-y: auto;
    padding-right: 4px;
  }

  .small {
    font-size: var(--step--1);
  }

  .sync {
    display: flex;
    gap: 4px;
  }

  .sync button {
    padding: 5px 10px;
  }

  .commit {
    display: grid;
    gap: 8px;
  }

  .commit textarea {
    width: 100%;
    min-height: 72px;
  }

  .commit-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 12px;
  }

  .commit-row button {
    flex: none;
    white-space: nowrap;
  }

  .group {
    display: grid;
    gap: 6px;
  }

  .group-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
  }

  h3 {
    font-size: var(--step-0);
    font-weight: 800;
  }

  .count {
    margin-left: 4px;
    color: var(--muted);
    font-weight: 400;
    font-variant-numeric: tabular-nums;
  }

  .group-head .link {
    font-size: var(--step--1);
    font-weight: 400;
  }

  .empty {
    padding: 6px 0;
  }

  .changes {
    min-width: 0;
    border-top: 1px solid var(--line);
    outline: none;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    min-height: 34px;
    padding: 0 0 0 calc(8px + var(--depth) * 16px);
    border-bottom: 1px solid var(--line);
    cursor: pointer;
    user-select: none;
  }

  .row:hover {
    background: var(--surface);
  }

  .row.selected {
    background: var(--here);
  }

  .row:focus-visible {
    outline: 2px solid var(--forest);
    outline-offset: -2px;
  }

  .folder {
    font-weight: 600;
  }

  .row .count {
    flex: none;
    font-size: var(--step--2);
  }

  .layout-row {
    display: flex;
    justify-content: end;
  }

  .badge {
    flex: none;
    width: 20px;
    text-align: center;
    font-family: var(--font-log);
    font-size: var(--step--2);
    font-weight: 700;
    border-radius: var(--radius-small);
    color: var(--muted);
    background: var(--highlight);
  }

  .badge.sA,
  .badge.sN {
    color: var(--forest-hi);
    background: var(--ok-fill);
  }

  .badge.sD {
    color: var(--fail);
    background: var(--fail-fill);
  }

  .badge.sM,
  .badge.sR {
    color: var(--warn);
    background: var(--warn-fill);
  }

  .path {
    flex: 1;
    display: flex;
    min-width: 0;
    white-space: nowrap;
    font-size: var(--step--1);
  }

  .dir {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--muted);
  }

  .name {
    flex: 0 0 auto;
    min-width: 0;
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .row-actions {
    flex: none;
    display: flex;
    gap: 2px;
    padding-right: 4px;
    color: var(--muted);
  }

  .row-actions .icon {
    color: inherit;
  }

  .row:hover .row-actions,
  .row:focus-within .row-actions,
  .row.selected .row-actions {
    color: var(--text);
  }

  .icon {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    padding: 0;
    font-size: var(--step-1);
    line-height: 1;
    background: transparent;
    border-color: transparent;
  }

  .icon:hover:not(:disabled) {
    background: var(--control);
    border-color: var(--control-edge);
  }

  .icon svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.6;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .git-log {
    display: flex;
    height: 180px;
  }

  .history {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .history li {
    display: grid;
    padding: 6px 0;
    border-bottom: 1px solid var(--line);
  }

  .subject {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .diff {
    display: flex;
    flex-direction: column;
    min-height: 0;
    border: 1px solid var(--line);
    border-radius: var(--radius-panel);
    background: var(--field);
    overflow: hidden;
  }

  .placeholder {
    display: grid;
    place-content: center;
    justify-items: center;
    gap: 4px;
    flex: 1;
    color: var(--muted);
  }

  .diff-head {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    min-width: 0;
    gap: 12px;
    padding: 10px 14px;
    background: var(--surface);
    border-bottom: 1px solid var(--line);
  }

  .path-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .state {
    flex: none;
  }

  .pad {
    padding: 14px;
  }

  .summary {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    align-content: start;
    gap: 14px;
    min-height: 0;
    overflow: auto;
  }

  .summary-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .summary-list {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .summary-list li {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    padding: 5px 0;
    font-size: var(--step--1);
    border-bottom: 1px solid var(--line);
  }

  .summary-list .path-text {
    flex: 1;
  }

  .selection-count {
    font-weight: 700;
  }
</style>
