<script lang="ts">
  import { SvelteSet } from 'svelte/reactivity'
  import { onMount, tick, untrack } from 'svelte'
  import { revealItemInDir } from '@tauri-apps/plugin-opener'
  import { getCurrentWebview, type DragDropEvent } from '@tauri-apps/api/webview'
  import { api, errorText, type DirEntry, type GitStatus, type PackKind, type Pasted } from '../lib/api'
  import { fs, moved, onDiskChange, treeState, treeUi } from '../lib/workspace.svelte'
  import { drag } from '../lib/drag.svelte'
  import { BACK_PRIORITY, useBack } from '../lib/back.svelte'
  import { paneFocus } from '../lib/panes.svelte'
  import { gitMenu } from '../lib/gitActions'
  import { RangeSelection, isPlainClick } from '../lib/rangeSelection'
  import { commandKey, fileManager, isMac, shortcut } from '../lib/platform'
  import { isGzip, outerArchive, packTarget } from '../lib/viewers'
  import BulkRename from './BulkRename.svelte'
  import ContextMenu from './ContextMenu.svelte'
  import type { OverrideItem } from './PackOverrideDialog.svelte'
  import Twisty from './Twisty.svelte'
  import type { MenuItem } from '../lib/menu'
  import type { RenameItem } from '../lib/bulkRename.svelte'

  let {
    selected,
    status,
    selection,
    hidden = false,
    onopen,
    ondelete,
    onremoved,
    ongitchange,
    onoverride,
  }: {
    selected: string | null
    status: GitStatus | null
    selection: SvelteSet<string>
    hidden?: boolean
    onopen: (path: string, pinned: boolean) => void
    ondelete: (paths: string[]) => void
    onremoved: (paths: string[]) => void
    ongitchange: () => void
    onoverride: (items: OverrideItem[]) => void
  } = $props()

  const DRAG_THRESHOLD = 5
  const EXPAND_DELAY = 600
  const SCROLL_EDGE = 28
  const SLOW_IMPORT = 400
  const MAX_EXPAND_ALL = 200
  const JUMP_RESET = 900

  const children = treeState.children
  const expanded = treeState.expanded
  const loading = new SvelteSet<string>()
  let focused = $state<string | null>(treeUi.focused)
  $effect(() => {
    treeUi.focused = focused
  })
  const picker = new RangeSelection(untrack(() => selection))
  let loadError = $state<string | null>(null)
  let message = $state<{ text: string; tone: 'fail' | 'note' } | null>(null)
  let edit = $state<{ kind: 'file' | 'folder' | 'rename'; parent: string; path: string | null; error: string | null; busy: boolean } | null>(null)
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null)
  let bulkRename = $state<RenameItem[] | null>(null)
  let cutPaths = $state<string[]>([])
  let jump = $state<{ text: string; found: boolean } | null>(null)
  let jumpTimer = 0
  let tree: HTMLDivElement = $state()!

  let press: { x: number; y: number; row: Row; pointerId: number; target: HTMLElement } | null = null
  let ghost = $state<{ x: number; y: number } | null>(null)
  let dropDir = $state<string | null>(null)
  let hover: { path: string; timer: number } | null = null
  let pointerX = 0
  let pointerY = 0
  let systemDrag = false
  let suppressClick = false

  type Row = { entry: DirEntry; depth: number; parent: string }
  type DraftLine = { draft: true; depth: number }

  function sameEntries(a: DirEntry[] | undefined, b: DirEntry[]): boolean {
    if (!a || a.length !== b.length) return false
    for (let i = 0; i < a.length; i++) {
      const x = a[i]
      const y = b[i]
      if (x.path !== y.path || x.name !== y.name || x.isDir !== y.isDir || x.size !== y.size || x.ignored !== y.ignored || x.archive !== y.archive || x.archived !== y.archived) return false
    }
    return true
  }

  async function load(dir: string) {
    loading.add(dir)
    try {
      const entries = await api.listDir(dir)
      if (!sameEntries(children.get(dir), entries)) children.set(dir, entries)
      const present = new Set(entries.map((e) => e.path))
      for (const path of selection) if (parentOf(path) === dir && !present.has(path)) selection.delete(path)
      loadError = null
    } catch (e) {
      loadError = errorText(e)
    } finally {
      loading.delete(dir)
    }
  }

  export function refresh() {
    return Promise.all(['', ...expanded].map(load))
  }

  export async function show(path: string, expand = false) {
    await revealInTree(path)
    if (expand) await expandDir(path)
    selectOnly(path)
    await focusRow(path)
  }

  export async function showMany(paths: string[]) {
    const dirs = new Set(paths.flatMap((p) => p.split('/').slice(0, -1).map((_, i, a) => a.slice(0, i + 1).join('/'))))
    const byDepth = [...dirs].sort((a, b) => a.split('/').length - b.split('/').length)
    for (const dir of byDepth) await expandDir(dir)
    picker.replace(paths)
    await focusRow(paths[0])
  }

  export function report(text: string, tone: 'fail' | 'note' = 'fail') {
    message = { text, tone }
  }

  export function forget(paths: string[]) {
    const gone = (path: string) => paths.some((p) => path === p || path.startsWith(`${p}/`))
    // Deleting the focused row would leave no row in the tab order, so we focus its neighbor.
    if (focused !== null && gone(focused)) {
      const at = rows.findIndex((r) => r.entry.path === focused)
      focused = null
      tick().then(() => {
        const next = rows[Math.min(Math.max(at, 0), rows.length - 1)]
        if (!next) return
        focused = next.entry.path
        if (document.activeElement === document.body || tree?.contains(document.activeElement)) {
          tree?.querySelector<HTMLElement>(`[data-path="${CSS.escape(next.entry.path)}"]`)?.focus()
        }
      })
    }
    for (const path of [...expanded]) if (gone(path)) expanded.delete(path)
    for (const dir of [...children.keys()]) if (gone(dir)) children.delete(dir)
    for (const path of [...selection]) if (gone(path)) selection.delete(path)
    cutPaths = cutPaths.filter((p) => !gone(p))
  }

  // Selected descendants of a selected folder go with the folder, so we leave them out.
  // Selected rows inside a collapsed folder aren't visible, so we leave those out too.
  export function selectedPaths() {
    const visible = rows.map((r) => r.entry.path).filter((p) => selection.has(p))
    return visible.filter((p) => !visible.some((other) => p.startsWith(`${other}/`)))
  }

  onMount(() => {
    if (children.has('')) refresh()
  })

  $effect(() => {
    if (!children.has('')) load('')
  })

  $effect(() => {
    if (!fs.seq) return
    for (const dir of fs.dirs) {
      if (untrack(() => children.has(dir))) load(dir)
    }
  })

  // Like VS Code, opening a file from a tab or elsewhere selects it in the tree.
  $effect(() => {
    const path = selected
    untrack(() => path && !selection.has(path) && selectOnly(path))
  })

  let lines = $derived.by(() => {
    const out: (Row | DraftLine)[] = []
    const creating = edit && edit.kind !== 'rename' ? edit.parent : null
    const walk = (dir: string, depth: number) => {
      if (creating === dir) out.push({ draft: true, depth })
      for (const entry of children.get(dir) ?? []) {
        out.push({ entry, depth, parent: dir })
        if (expandable(entry) && expanded.has(entry.path)) walk(entry.path, depth + 1)
      }
    }
    walk('', 0)
    return out
  })

  let rows = $derived(lines.filter((line): line is Row => !('draft' in line)))
  let tabStop = $derived(rows.some((r) => r.entry.path === focused) ? focused : (rows[0]?.entry.path ?? null))
  let entries = $derived(new Map(rows.map((r) => [r.entry.path, r.entry])))

  let changes = $derived.by(() => {
    const map = new Map<string, string>()
    for (const c of status?.staged ?? []) map.set(c.path, c.status)
    for (const c of status?.unstaged ?? []) map.set(c.path, c.status)
    return map
  })

  let changedDirs = $derived.by(() => {
    const dirs = new Set<string>()
    for (const path of changes.keys()) {
      let index = path.lastIndexOf('/')
      while (index > 0) {
        dirs.add(path.slice(0, index))
        index = path.lastIndexOf('/', index - 1)
      }
    }
    return dirs
  })

  function parentOf(path: string) {
    const index = path.lastIndexOf('/')
    return index < 0 ? '' : path.slice(0, index)
  }

  function nameOf(path: string) {
    return path.slice(path.lastIndexOf('/') + 1)
  }

  function expandable(entry: DirEntry) {
    return entry.isDir || entry.archive
  }

  function readOnly(paths: string[]) {
    return paths.some((p) => entries.get(p)?.archived)
  }

  function explainReadOnly(path: string) {
    const target = packTarget(path)
    const how = target ? `Override them in a ${target.kind === 'resource' ? 'resource' : 'data'} pack to edit a copy.` : 'Extract it to change them.'
    report(`Files inside ${nameOf(outerArchive(path))} are read-only. ${how}`, 'note')
  }

  /** Returns the selected entries inside a mod jar that can be overridden in a pack. */
  export function selectedOverrides(kind?: PackKind): OverrideItem[] {
    return selectedPaths().flatMap((path) => {
      const entry = entries.get(path)
      const target = entry?.archived && !entry.archive ? packTarget(path) : null
      return entry && target && (!kind || target.kind === kind) ? [{ source: path, isDir: entry.isDir }] : []
    })
  }

  /** Returns the paths you can change, leaving out entries inside jars and zips. */
  export function writable(paths: string[]) {
    return paths.filter((p) => !entries.get(p)?.archived)
  }

  function targetDir(row: Row | null) {
    if (!row) return ''
    return row.entry.isDir ? row.entry.path : row.parent
  }

  function toggle(entry: DirEntry, open = !expanded.has(entry.path)) {
    if (!expandable(entry)) return
    if (open) expandDir(entry.path)
    else expanded.delete(entry.path)
  }

  function expandDir(dir: string) {
    if (!dir) return
    expanded.add(dir)
    if (!children.has(dir)) return load(dir)
  }

  // We expand one level at a time, so a deep folder can't start hundreds of listings at once.
  async function expandAll(dir: string) {
    let level = [dir]
    let count = 0
    while (level.length && count < MAX_EXPAND_ALL) {
      level = level.slice(0, MAX_EXPAND_ALL - count)
      count += level.length
      await Promise.all(level.map((path) => expandDir(path)))
      level = level.flatMap((path) => (children.get(path) ?? []).filter((e) => e.isDir).map((e) => e.path))
    }
    if (level.length) report(`We stopped after expanding ${MAX_EXPAND_ALL} folders.`, 'note')
  }

  function collapseAll(dir: string) {
    for (const path of [...expanded]) if (!dir || path === dir || path.startsWith(`${dir}/`)) expanded.delete(path)
    if (focused && !rows.some((r) => r.entry.path === focused)) focusRow(dir || rows[0]?.entry.path)
  }

  // Like Explorer, typing a name's first letters jumps to it, and repeating one letter cycles
  // through the names that start with it.
  function jumpTo(key: string, from: number) {
    clearTimeout(jumpTimer)
    const text = (jump?.text ?? '') + key.toLowerCase()
    const find = (prefix: string, start: number) => [...rows.slice(start), ...rows.slice(0, start)].find((r) => r.entry.name.toLowerCase().startsWith(prefix))
    const repeated = [...text].every((c) => c === text[0])
    const match = (text.length > 1 && find(text, from)) || (repeated && find(text[0], from + 1)) || null
    jump = { text, found: !!match }
    jumpTimer = setTimeout(() => (jump = null), JUMP_RESET)
    if (!match) return
    selectOnly(match.entry.path)
    focusRow(match.entry.path)
  }

  async function revealInTree(path: string) {
    const parts = path.split('/')
    for (let i = 1; i < parts.length; i++) await expandDir(parts.slice(0, i).join('/'))
  }

  function activate(row: Row) {
    focused = row.entry.path
    if (expandable(row.entry)) toggle(row.entry)
    else onopen(row.entry.path, false)
  }

  function selectOnly(path: string) {
    picker.only(path)
  }

  function selectRange(to: string) {
    picker.range(rows.map((r) => r.entry.path), to)
  }

  function fromNameField(event: Event) {
    return event.target instanceof HTMLInputElement
  }

  function onRowClick(event: MouseEvent, row: Row) {
    if (suppressClick || fromNameField(event)) return
    const path = row.entry.path
    focused = path
    picker.click(event, path, rows.map((r) => r.entry.path))
    if (isPlainClick(event)) activate(row)
  }

  function onRowDoubleClick(event: MouseEvent, row: Row) {
    if (suppressClick || fromNameField(event) || row.entry.isDir || !isPlainClick(event)) return
    onopen(row.entry.path, true)
  }

  async function focusRow(path: string | undefined) {
    if (!path) return
    focused = path
    await tick()
    tree.querySelector<HTMLElement>(`[data-path="${CSS.escape(path)}"]`)?.focus()
  }

  function onKey(event: KeyboardEvent) {
    if (fromNameField(event)) return
    const index = rows.findIndex((r) => r.entry.path === focused)
    const row = rows[index]
    if (!row) return
    const move = (to: number) => {
      const target = rows[to]?.entry.path
      if (!target) return
      if (event.shiftKey) selectRange(target)
      else selectOnly(target)
      focusRow(target)
    }
    const key = event.key.toLowerCase()
    if (commandKey(event) && key === 'a') {
      for (const r of rows) selection.add(r.entry.path)
    } else if (commandKey(event) && (key === 'c' || key === 'x')) {
      if (readOnly(selectedPaths())) explainReadOnly(row.entry.path)
      else copy(selectedPaths(), key === 'x')
    } else if (commandKey(event) && (key === 'y' || (key === 'z' && event.shiftKey))) {
      stepHistory(true)
    } else if (commandKey(event) && key === 'z') {
      stepHistory(false)
    } else if (commandKey(event) && key === 'v') {
      if (row.entry.archived) explainReadOnly(row.entry.path)
      else paste(targetDir(row))
    } else if (event.key === 'F2') {
      const paths = selectedPaths()
      if (readOnly(paths.length ? paths : [row.entry.path])) explainReadOnly(row.entry.path)
      else if (paths.length > 1) startBulkRename(paths)
      else startRename(row.entry.path)
    } else if (event.key === 'Delete' || (isMac && event.metaKey && event.key === 'Backspace')) {
      if (readOnly(selectedPaths())) explainReadOnly(row.entry.path)
      else ondelete(selectedPaths())
    } else if (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) {
      const rect = (event.target as HTMLElement).getBoundingClientRect()
      openMenu(rect.left + 24, rect.bottom, row)
    } else if (event.key === 'ArrowDown') {
      move(index + 1)
    } else if (event.key === 'ArrowUp') {
      move(index - 1)
    } else if (event.key === 'Home') {
      move(0)
    } else if (event.key === 'End') {
      move(rows.length - 1)
    } else if (event.key === 'ArrowRight') {
      if (expandable(row.entry) && !expanded.has(row.entry.path)) toggle(row.entry, true)
      else if (expandable(row.entry)) move(index + 1)
    } else if (event.key === 'ArrowLeft') {
      if (expandable(row.entry) && expanded.has(row.entry.path)) toggle(row.entry, false)
      else if (row.parent) move(rows.findIndex((r) => r.entry.path === row.parent))
    } else if (event.key === '*' && row.entry.isDir) {
      expandAll(row.entry.path)
    } else if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey && (event.key !== ' ' || jump)) {
      jumpTo(event.key, index)
    } else if (event.key === 'Enter' || event.key === ' ') {
      selectOnly(row.entry.path)
      activate(row)
    } else {
      return
    }
    event.preventDefault()
  }

  function onContextMenu(event: MouseEvent, row: Row | null) {
    if (fromNameField(event)) return
    event.preventDefault()
    event.stopPropagation()
    if (row && !selection.has(row.entry.path)) selectOnly(row.entry.path)
    if (row) focused = row.entry.path
    // The keyboard menu key fires contextmenu without pointer coordinates.
    if (row && !event.clientX && !event.clientY) {
      const rect = (event.currentTarget as HTMLElement).getBoundingClientRect()
      openMenu(rect.left + 24, rect.bottom, row)
    } else {
      openMenu(event.clientX, event.clientY, row)
    }
  }

  async function openMenu(x: number, y: number, row: Row | null) {
    if (row?.entry.archived) {
      menu = { x, y, items: archivedMenu(selectedPaths()) }
      return
    }
    const history = await api.fileHistory().catch(() => null)
    const dir = targetDir(row)
    const paths = row ? selectedPaths() : []
    const count = paths.length > 1 ? ` ${paths.length} items` : ''
    const single = paths.length === 1 ? rows.find((r) => r.entry.path === paths[0])?.entry : undefined
    const folder = single?.isDir ? single.path : row ? null : ''
    const git = await gitMenu(paths, status, (path) => rows.find((r) => r.entry.path === path)?.entry.ignored ?? false, {
      ondone: ongitchange,
      onerror: (text) => report(text),
      onnotice: (text) => report(text, 'note'),
    })
    menu = {
      x,
      y,
      items: [
        ...(history?.undo ? [{ label: `Undo ${history.undo}`, shortcut: shortcut('Ctrl+Z'), run: () => stepHistory(false) }] : []),
        ...(history?.redo ? [{ label: `Redo ${history.redo}`, shortcut: shortcut(isMac ? 'Ctrl+Shift+Z' : 'Ctrl+Y'), run: () => stepHistory(true) }] : []),
        'separator',
        { label: 'New file', run: () => startCreate(dir, 'file') },
        { label: 'New folder', run: () => startCreate(dir, 'folder') },
        'separator',
        ...(paths.length
          ? [
              { label: `Cut${count}`, shortcut: shortcut('Ctrl+X'), run: () => copy(paths, true) },
              { label: `Copy${count}`, shortcut: shortcut('Ctrl+C'), run: () => copy(paths) },
            ]
          : []),
        { label: 'Paste', shortcut: shortcut('Ctrl+V'), run: () => paste(dir) },
        ...(paths.length === 1 ? [{ label: 'Rename', shortcut: 'F2', run: () => startRename(paths[0]) }] : []),
        ...(paths.length > 1 ? [{ label: `Rename${count}…`, shortcut: 'F2', run: () => startBulkRename(paths) }] : []),
        'separator',
        ...(single && (single.archive || (!single.isDir && isGzip(single.path))) ? [{ label: 'Extract here', run: () => extract(single.path) }] : []),
        ...(paths.length && !single?.archive ? [{ label: paths.length > 1 ? `Compress ${paths.length} items to .zip` : 'Compress to .zip', run: () => compress(paths) }] : []),
        'separator',
        ...(paths.length
          ? [
              { label: paths.length > 1 ? 'Copy paths' : 'Copy path', run: () => copyPaths(paths, true) },
              { label: paths.length > 1 ? 'Copy relative paths' : 'Copy relative path', run: () => copyPaths(paths, false) },
            ]
          : []),
        'separator',
        ...(folder ? [{ label: 'Expand all', shortcut: '*', run: () => expandAll(folder) }] : []),
        ...(folder !== null ? [{ label: 'Collapse all', disabled: !expanded.size, run: () => collapseAll(folder) }] : []),
        'separator',
        ...(git ? [git] : []),
        ...(single && !single.isDir ? [{ label: 'Open in default app', run: () => openWithDefaultApp(single.path) }] : []),
        ...(paths.length === 1 ? [{ label: `Show in ${fileManager}`, run: () => reveal(paths[0]) }] : []),
        'separator',
        ...(paths.length ? [{ label: `Delete${count}`, shortcut: shortcut('Delete'), danger: true, run: () => ondelete(paths) }] : []),
      ],
    }
  }

  // Entries inside a jar or zip are read-only, so their menu only offers ways to look at them.
  function archivedMenu(paths: string[]): MenuItem[] {
    const archived = paths.filter((p) => entries.get(p)?.archived)
    const single = archived.length === 1 ? entries.get(archived[0]) : undefined
    const inside = (path: string) => path.slice(outerArchive(path).length + 1)
    const overrides = (kind: PackKind): OverrideItem[] =>
      archived.flatMap((path) => {
        const entry = entries.get(path)
        return entry && !entry.archive && packTarget(path)?.kind === kind ? [{ source: path, isDir: entry.isDir }] : []
      })
    const data = overrides('data')
    const resources = overrides('resource')
    const archives = [...new Set(archived.map(outerArchive))]
    return [
      ...(single && !single.isDir && !single.archive ? [{ label: 'Open', run: () => onopen(single.path, true) }] : []),
      'separator',
      ...(data.length ? [{ label: data.length > 1 ? `Override ${data.length} items in a data pack…` : 'Override in a data pack…', run: () => onoverride(data) }] : []),
      ...(resources.length
        ? [{ label: resources.length > 1 ? `Override ${resources.length} items in a resource pack…` : 'Override in a resource pack…', run: () => onoverride(resources) }]
        : []),
      'separator',
      { label: archived.length > 1 ? 'Copy paths inside the archive' : 'Copy path inside the archive', run: () => copyPaths(archived.map(inside), false) },
      'separator',
      ...(single?.isDir ? [{ label: 'Expand all', shortcut: '*', run: () => expandAll(single.path) }] : []),
      ...(single && expandable(single) ? [{ label: 'Collapse all', run: () => collapseAll(single.path) }] : []),
      'separator',
      ...(archives.length === 1 ? [{ label: `Show the archive in ${fileManager}`, run: () => reveal(archives[0]) }] : []),
    ]
  }

  export async function extract(path: string) {
    message = null
    try {
      const folder = await api.extractArchive(path)
      await load(parentOf(folder))
      await show(folder)
    } catch (e) {
      report(errorText(e))
    }
  }

  async function compress(paths: string[]) {
    message = null
    const slow = setTimeout(() => report('Compressing…', 'note'), SLOW_IMPORT)
    try {
      const zip = await api.compressPaths(paths)
      await load(parentOf(zip))
      message = null
      await show(zip)
    } catch (e) {
      report(errorText(e))
    } finally {
      clearTimeout(slow)
    }
  }

  async function startCreate(parent: string, kind: 'file' | 'folder') {
    message = null
    await expandDir(parent)
    edit = { kind, parent, path: null, error: null, busy: false }
  }

  function startRename(path: string) {
    message = null
    edit = { kind: 'rename', parent: parentOf(path), path, error: null, busy: false }
  }

  function startBulkRename(paths: string[]) {
    message = null
    bulkRename = paths.flatMap((path) => {
      const entry = entries.get(path)
      return entry ? [{ path, name: entry.name, isDir: entry.isDir, size: entry.size }] : []
    })
  }

  function siblingsOf(dir: string) {
    return (children.get(dir) ?? []).map((e) => e.name)
  }

  function closeBulkRename() {
    bulkRename = null
    if (focused) focusRow(focused)
  }

  async function afterBulkRename(pairs: [string, string][]) {
    bulkRename = null
    if (!pairs.length) return
    await afterMove(pairs)
    picker.replace(pairs.map(([, to]) => to))
    focusRow(pairs[0][1])
    report(`Renamed ${pairs.length === 1 ? nameOf(pairs[0][0]) : `${pairs.length} items`}. Press ${shortcut('Ctrl+Z')} here to undo.`, 'note')
  }

  function nameField(input: HTMLInputElement) {
    if (edit?.kind === 'rename' && edit.path) {
      const name = nameOf(edit.path)
      input.value = name
      const isDir = rows.find((r) => r.entry.path === edit!.path)?.entry.isDir
      const dot = name.lastIndexOf('.')
      input.setSelectionRange(0, isDir || dot <= 0 ? name.length : dot)
    }
    input.focus()
  }

  async function commitEdit(input: HTMLInputElement, fromBlur: boolean) {
    if (!edit || edit.busy) return
    const name = input.value.trim()
    const unchanged = edit.kind === 'rename' && edit.path && name === nameOf(edit.path)
    if (!name || unchanged || (fromBlur && edit.error)) {
      edit = null
      return
    }
    const { kind, parent, path: from } = edit
    const invalid = name.includes('\\') || (kind === 'rename' && name.includes('/'))
    const path = parent ? `${parent}/${name}` : name
    edit.busy = true
    try {
      if (invalid) throw kind === 'rename' ? "Names can't contain / or \\." : "Use / to separate folders, not \\."
      if (kind === 'rename') await api.renamePath(from!, path)
      else await (kind === 'file' ? api.createFile(path) : api.createDir(path))
    } catch (e) {
      if (fromBlur) {
        edit = null
        report(errorText(e))
      } else {
        edit.error = errorText(e)
        edit.busy = false
        await tick()
        input.focus()
      }
      return
    }
    edit = null
    if (kind === 'rename') {
      await afterMove([[from!, path]])
    } else {
      await load(parent)
      await revealInTree(path)
    }
    selectOnly(path)
    if (kind === 'file') onopen(path, true)
    else focusRow(path)
  }

  function onNameKey(event: KeyboardEvent) {
    const input = event.currentTarget as HTMLInputElement
    if (event.key === 'Enter') {
      event.preventDefault()
      commitEdit(input, false)
    } else if (event.key === 'Escape') {
      event.preventDefault()
      edit = null
      if (focused) focusRow(focused)
    } else if (edit?.error) {
      edit.error = null
    }
  }

  const inFocusedPane = paneFocus()

  useBack(BACK_PRIORITY.prompt, () => {
    if (!inFocusedPane()) return false
    if (drag.paths.length) {
      cancelDrag()
      return true
    }
    if (edit) {
      edit = null
      if (focused) focusRow(focused)
      return true
    }
    return false
  })

  useBack(BACK_PRIORITY.selection, () => {
    if (!inFocusedPane() || selection.size <= 1) return false
    selectOnly(focused ?? [...selection][0])
    return true
  })

  // A moved folder takes its expanded subfolders with it, so we carry them over to the new
  // paths and reload everything that's open.
  async function afterMove(pairs: [string, string][]) {
    await moved(pairs)
    for (const path of [...expanded]) {
      for (const [from, to] of pairs) {
        if (path === from || path.startsWith(`${from}/`)) {
          expanded.delete(path)
          expanded.add(to + path.slice(from.length))
        }
      }
    }
    for (const dir of [...children.keys()]) if (pairs.some(([from]) => dir === from || dir.startsWith(`${from}/`))) children.delete(dir)
    await refresh()
  }

  // Like Explorer, Ctrl+Z in the tree reverses the last file operation rather than an edit.
  async function stepHistory(redo: boolean) {
    message = null
    try {
      const outcome = await api.fileUndo(redo)
      if (!outcome) {
        report(redo ? 'Nothing to redo.' : 'Nothing to undo.', 'note')
        return
      }
      if (outcome.removed.length) {
        onremoved(outcome.removed)
        forget(outcome.removed)
      }
      if (outcome.changed.length) await onDiskChange(outcome.changed)
      if (outcome.moved.length) await afterMove(outcome.moved)
      else await refresh()
      const shown = [...outcome.added, ...outcome.moved.map(([, to]) => to)]
      for (const path of shown) await revealInTree(path)
      picker.replace(shown)
      if (shown[0]) focusRow(shown[0])
      report(`${redo ? 'Redid' : 'Undid'}: ${outcome.label}`, 'note')
    } catch (e) {
      report(errorText(e))
    }
  }

  async function moveInto(paths: string[], dir: string) {
    message = null
    try {
      const pairs = await api.movePaths(paths, dir)
      await expandDir(dir)
      await afterMove(pairs)
      picker.replace(pairs.map(([, to]) => to))
    } catch (e) {
      report(errorText(e))
    }
  }

  async function copy(paths: string[], cut = false) {
    if (!paths.length) return
    message = null
    try {
      await api.copyFiles(paths, cut)
      cutPaths = cut ? paths : []
    } catch (e) {
      report(errorText(e))
    }
  }

  async function paste(dir: string) {
    message = null
    try {
      const pasted = await api.pasteFiles(dir, cutPaths)
      cutPaths = []
      await showPasted(dir, pasted)
    } catch (e) {
      report(errorText(e))
    }
  }

  async function showPasted(dir: string, { created, moved }: Pasted) {
    await expandDir(dir)
    if (moved.length) await afterMove(moved)
    else await load(dir)
    picker.replace(created)
    if (created[0]) focusRow(created[0])
  }

  async function importInto(dir: string, sources: string[]) {
    message = null
    const name = sources[0].split(/[\\/]/).pop()
    const slow = setTimeout(() => report(sources.length === 1 ? `Adding ${name}…` : `Adding ${sources.length} items…`, 'note'), SLOW_IMPORT)
    try {
      await showPasted(dir, await api.importPaths(dir, sources))
      message = null
    } catch (e) {
      report(errorText(e))
    } finally {
      clearTimeout(slow)
    }
  }

  // Tauri reports drags from Explorer or Finder in physical pixels, so we convert them to CSS
  // pixels before finding the folder under the pointer.
  function onSystemDrag({ payload }: { payload: DragDropEvent }) {
    if (payload.type === 'leave') {
      endSystemDrag()
      return
    }
    locate(payload.position.x / devicePixelRatio, payload.position.y / devicePixelRatio, false)
    if (payload.type === 'drop') {
      const dir = dropDir
      endSystemDrag()
      if (dir !== null && payload.paths.length) importInto(dir, payload.paths)
    } else if (!systemDrag) {
      systemDrag = true
      requestAnimationFrame(autoScroll)
    }
  }

  function endSystemDrag() {
    systemDrag = false
    dropDir = null
    expandAfterHover(null)
  }

  onMount(() => {
    const unlisten = getCurrentWebview().onDragDropEvent(onSystemDrag)
    return () => {
      unlisten.then((stop) => stop())
    }
  })

  async function copyPaths(paths: string[], absolute: boolean) {
    try {
      await api.copyPaths(paths, absolute)
    } catch (e) {
      report(errorText(e))
    }
  }

  async function openWithDefaultApp(path: string) {
    try {
      await api.openWithDefaultApp(path)
    } catch (e) {
      report(errorText(e))
    }
  }

  async function reveal(path: string) {
    try {
      await revealItemInDir(await api.absolutePath(path))
    } catch (e) {
      report(errorText(e))
    }
  }

  // We drag with pointer events rather than HTML drag and drop, because Tauri's native file
  // drop handling disables HTML drag and drop in WebView2.
  function onPointerDown(event: PointerEvent, row: Row) {
    if (event.button !== 0 || fromNameField(event)) return
    press = { x: event.clientX, y: event.clientY, row, pointerId: event.pointerId, target: event.currentTarget as HTMLElement }
  }

  function onPointerMove(event: PointerEvent) {
    if (!press) return
    if (!drag.paths.length) {
      if (Math.hypot(event.clientX - press.x, event.clientY - press.y) < DRAG_THRESHOLD) return
      if (press.row.entry.archived) {
        explainReadOnly(press.row.entry.path)
        press = null
        return
      }
      startDrag(press)
    }
    if (event.clientX < 0 || event.clientY < 0 || event.clientX >= innerWidth || event.clientY >= innerHeight) dragOut()
    else trackDrag(event)
  }

  // Once you drag past the window's edge, we hand the drag to the OS, so you can drop the files
  // into Explorer, Finder, Discord or anywhere else that takes files. The drop target decides
  // whether to move or copy them, so we reload the tree afterwards.
  function dragOut() {
    const paths = drag.paths
    press = null
    endDrag()
    api
      .dragOut(paths)
      .then(refresh)
      .catch((e) => report(errorText(e)))
  }

  function startDrag({ row, target, pointerId }: NonNullable<typeof press>) {
    if (!selection.has(row.entry.path)) selectOnly(row.entry.path)
    drag.paths = selectedPaths()
    target.setPointerCapture(pointerId)
    requestAnimationFrame(autoScroll)
  }

  function canMoveInto(dir: string) {
    return drag.paths.every((p) => dir !== p && !dir.startsWith(`${p}/`)) && drag.paths.some((p) => parentOf(p) !== dir)
  }

  function trackDrag(event: PointerEvent) {
    ghost = { x: event.clientX, y: event.clientY }
    locate(event.clientX, event.clientY, true)
  }

  // Files dragged in from the OS can't go in the trash, and the backend checks whether they
  // can go where you drop them.
  function locate(x: number, y: number, internal: boolean) {
    pointerX = x
    pointerY = y
    const under = document.elementFromPoint(x, y)
    drag.overTrash = internal && !!under?.closest('[data-drop="trash"]')
    const inTree = !drag.overTrash && !!under && tree.contains(under)
    const path = inTree ? under!.closest<HTMLElement>('[data-path]')?.dataset.path : undefined
    const row = path ? (rows.find((r) => r.entry.path === path) ?? null) : null
    const dir = inTree && !row?.entry.archived ? targetDir(row) : null
    dropDir = dir !== null && (!internal || canMoveInto(dir)) ? dir : null
    expandAfterHover(row?.entry.isDir && !row.entry.archived && !expanded.has(row.entry.path) ? row.entry.path : null)
  }

  function expandAfterHover(path: string | null) {
    if (hover?.path === path) return
    if (hover) clearTimeout(hover.timer)
    hover = path ? { path, timer: setTimeout(() => expandDir(path), EXPAND_DELAY) } : null
  }

  function autoScroll() {
    if (!drag.paths.length && !systemDrag) return
    const rect = tree.getBoundingClientRect()
    const overColumn = pointerX >= rect.left && pointerX <= rect.right
    if (overColumn && pointerY < rect.top + SCROLL_EDGE) tree.scrollTop -= 8
    else if (overColumn && pointerY > rect.bottom - SCROLL_EDGE) tree.scrollTop += 8
    requestAnimationFrame(autoScroll)
  }

  function endDrag() {
    drag.paths = []
    drag.overTrash = false
    dropDir = null
    ghost = null
    expandAfterHover(null)
  }

  function onPointerUp() {
    press = null
    if (!drag.paths.length) return
    const paths = drag.paths
    const overTrash = drag.overTrash
    const dir = dropDir
    endDrag()
    suppressClick = true
    setTimeout(() => (suppressClick = false))
    if (overTrash) ondelete(paths)
    else if (dir !== null) moveInto(paths, dir)
  }

  function cancelDrag() {
    press = null
    if (drag.paths.length) endDrag()
  }

  function badge(path: string) {
    const status = changes.get(path)
    return status === '?' ? 'N' : status
  }
</script>

<svelte:window
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={cancelDrag}
  onkeydown={(e) => e.key === 'Escape' && drag.paths.length && (e.preventDefault(), cancelDrag())}
/>

{#snippet nameInput()}
  <input
    use:nameField
    class="name-field"
    aria-label={edit?.kind === 'rename' ? 'New name' : `New ${edit?.kind} name`}
    aria-invalid={!!edit?.error}
    placeholder={edit?.kind === 'file' ? 'name.json' : edit?.kind === 'folder' ? 'folder name' : undefined}
    spellcheck="false"
    disabled={edit?.busy}
    onkeydown={onNameKey}
    onblur={(e) => commitEdit(e.currentTarget, true)}
  />
{/snippet}

<div
  class="tree"
  class:hidden
  class:dragging={!!ghost}
  class:drop-root={dropDir === ''}
  role="tree"
  aria-label="Pack files"
  aria-multiselectable="true"
  bind:this={tree}
  tabindex="-1"
  onkeydown={onKey}
  oncontextmenu={(e) => onContextMenu(e, null)}
>
  {#if loadError}<p class="failed small">{loadError}</p>{/if}
  {#if message}
    <p class="message" class:failed={message.tone === 'fail'} class:muted={message.tone === 'note'}>
      <span>{message.text}</span>
      <button class="dismiss" aria-label="Dismiss" onclick={() => (message = null)}>×</button>
    </p>
  {/if}
  {#each lines as line ('draft' in line ? '\0draft' : line.entry.path)}
    {#if 'draft' in line}
      <div class="row editing" style="--depth: {line.depth}">
        <Twisty open={null} />
        {@render nameInput()}
      </div>
      {#if edit?.error}<p class="failed small edit-error" style="--depth: {line.depth}">{edit.error}</p>{/if}
    {:else}
      {@const row = line}
      {@const status = row.entry.isDir ? null : badge(row.entry.path)}
      {@const renaming = edit?.kind === 'rename' && edit.path === row.entry.path}
      <!-- svelte-ignore a11y_click_events_have_key_events (the tree container handles keys) -->
      <div
        role="treeitem"
        aria-level={row.depth + 1}
        aria-expanded={expandable(row.entry) ? expanded.has(row.entry.path) : undefined}
        aria-selected={selection.has(row.entry.path)}
        tabindex={row.entry.path === tabStop ? 0 : -1}
        data-path={row.entry.path}
        class="row"
        class:selected={selection.has(row.entry.path)}
        class:ignored={row.entry.ignored}
        class:archived={row.entry.archived}
        title={row.entry.archived ? `Inside ${nameOf(outerArchive(row.entry.path))}, read-only` : undefined}
        class:cut={cutPaths.includes(row.entry.path)}
        class:editing={renaming}
        class:drop={dropDir !== null && dropDir === row.entry.path}
        style="--depth: {row.depth}"
        onclick={(e) => onRowClick(e, row)}
        ondblclick={(e) => onRowDoubleClick(e, row)}
        oncontextmenu={(e) => onContextMenu(e, row)}
        onpointerdown={(e) => onPointerDown(e, row)}
        onlostpointercapture={cancelDrag}
        onfocus={() => (focused = row.entry.path)}
      >
        <Twisty open={expandable(row.entry) ? expanded.has(row.entry.path) : null} />
        {#if renaming}
          {@render nameInput()}
        {:else}
          <span class="name" class:dir={row.entry.isDir}>{row.entry.name}</span>
        {/if}
        {#if loading.has(row.entry.path)}<span class="small muted">loading</span>{/if}
        {#if status}
          <span class="status s{status}" title="Changed in git">{status}</span>
        {:else if row.entry.isDir && changedDirs.has(row.entry.path)}
          <span class="dot" title="Contains changes"></span>
        {/if}
      </div>
      {#if renaming && edit?.error}<p class="failed small edit-error" style="--depth: {row.depth}">{edit.error}</p>{/if}
    {/if}
  {/each}
  {#if jump}
    <p class="jump" class:miss={!jump.found}>
      {jump.found ? 'Jump to' : 'No name starts with'} <kbd>{jump.text}</kbd>
    </p>
  {/if}
</div>

<span class="visually-hidden" role="status">{message?.text ?? ''}{jump ? ` ${jump.found ? 'Jump to' : 'No name starts with'} ${jump.text}` : ''}</span>

{#if ghost}
  <div class="ghost" style:left="{ghost.x + 14}px" style:top="{ghost.y + 10}px" aria-hidden="true">
    {drag.paths.length === 1 ? nameOf(drag.paths[0]) : `${drag.paths.length} items`}
  </div>
{/if}

{#if bulkRename}
  <BulkRename items={bulkRename} {siblingsOf} onclose={closeBulkRename} ondone={afterBulkRename} />
{/if}

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} label="File actions" items={menu.items} scroller={tree} onclose={() => (menu = null)} />
{/if}

<style>
  .tree {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 4px 0;
    font-size: var(--step--1);
    outline: none;
  }

  .tree.hidden {
    display: none;
  }

  .tree.drop-root {
    box-shadow: inset 0 0 0 2px var(--forest);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 3px 10px 3px calc(8px + var(--depth) * 16px);
    white-space: nowrap;
    cursor: pointer;
    user-select: none;
    content-visibility: auto;
    contain-intrinsic-size: auto 23px;
  }

  .row:hover {
    background: var(--surface);
  }

  .row:focus-visible {
    outline: 2px solid var(--forest);
    outline-offset: -2px;
  }

  .row.selected {
    background: var(--here);
  }

  .row.drop {
    background: var(--ok-fill);
    box-shadow: inset 0 0 0 1px var(--ok-border);
  }

  .tree.dragging .row {
    cursor: grabbing;
  }

  /* Entries inside a jar or zip take a warm tint, since you can look but not change them. */
  .row.archived:not(.selected):not(:hover) {
    background: color-mix(in srgb, var(--ochre) 14%, transparent);
  }

  .row.archived .name {
    color: var(--ochre-ink);
  }

  .row.cut .name {
    color: var(--muted);
    font-style: italic;
  }

  .row.ignored .name {
    color: var(--muted);
  }

  .row.editing {
    padding-block: 1px;
    cursor: default;
  }

  .name-field {
    flex: 1;
    min-width: 0;
    padding: 1px 6px;
    font-size: var(--step--1);
    border-color: var(--forest);
  }

  .name-field[aria-invalid='true'] {
    border-color: var(--fail);
  }

  .edit-error {
    padding: 2px 10px 4px calc(26px + var(--depth) * 16px);
    white-space: normal;
  }

  .message {
    position: sticky;
    top: 4px;
    z-index: 2;
    display: flex;
    align-items: start;
    gap: 6px;
    margin: 2px 6px 6px;
    padding: 4px 4px 4px 8px;
    font-size: var(--step--2);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-control);
    background: var(--surface);
    box-shadow: 0 4px 12px var(--shadow);
  }

  .message span {
    flex: 1;
  }

  .dismiss {
    flex: none;
    padding: 0 6px;
    font-weight: 400;
    color: var(--muted);
    background: transparent;
    border-color: transparent;
  }

  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .name.dir {
    font-weight: 600;
  }

  .status {
    flex: none;
    font-family: var(--font-log);
    font-size: var(--step--2);
    font-weight: 700;
    color: var(--warn);
  }

  .status.sA,
  .status.sN {
    color: var(--forest);
  }

  .status.sD {
    color: var(--fail);
  }

  .dot {
    flex: none;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--warn);
  }

  .small {
    font-size: var(--step--2);
    padding: 0 10px;
  }

  /* The jump hint floats at the bottom of the tree rather than looking like a search field,
     since it only moves the selection and never filters. */
  .jump {
    position: sticky;
    bottom: 6px;
    width: fit-content;
    margin: 6px auto 0;
    padding: 2px 10px;
    font-size: var(--step--2);
    color: var(--muted);
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: 999px;
    box-shadow: 0 4px 12px var(--shadow);
    pointer-events: none;
  }

  .jump.miss kbd {
    color: var(--fail);
  }

  .ghost {
    position: fixed;
    z-index: 30;
    max-width: 240px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    padding: 2px 8px;
    font-size: var(--step--1);
    font-weight: 700;
    color: var(--text);
    background: var(--here);
    border: 1px solid var(--here-border);
    border-radius: var(--radius-control);
    box-shadow: 0 6px 16px var(--shadow);
    pointer-events: none;
  }
</style>
