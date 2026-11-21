import type { EditorState, TransactionSpec } from '@codemirror/state'
import { SvelteMap, SvelteSet } from 'svelte/reactivity'
import type { Problems } from './editor'
import { listen } from '@tauri-apps/api/event'
import { api, type DirEntry, type FileContent, type GitStatus } from './api'
import { resetProbejs } from './probejs.svelte'
import { resetKubejsContext } from './kubejsContext'
import { isGzip, isLog, outerArchive } from './viewers'

export interface Tab {
  path: string
  state: EditorState | null
  saved: string
  dirty: boolean
  crlf: boolean
  bom: boolean
  unreadable: string | null
  conflict: 'changed' | 'deleted' | null
  disk: string | null
  problems: Problems
  preview: boolean
  archived: boolean
  compressed: boolean
  /** Over the size you set to ask about, so its text, once loaded, shows in the simplified editor. */
  large: boolean
  lintPaused: boolean
}

// blocked counts attempts to edit a read-only tab, so the view can say why nothing happened.
export const workspace = $state({ tabs: [] as Tab[], active: null as string | null, pack: null as string | null, error: null as string | null, blocked: 0 })

export const fs = $state({ seq: 0, dirs: [] as string[] })

// We keep the file tree's loaded folders and expanded state for the active pack, so switching
// back to Files shows the tree right away while refreshing in the background.
export const treeState = {
  children: new SvelteMap<string, DirEntry[]>(),
  expanded: new SvelteSet<string>(),
}
export const treeUi = $state({ focused: null as string | null, status: null as GitStatus | null })

const MAX_CLOSED = 20
const closed: string[] = []

const bridge: { path: string | null; dispatch: ((spec: TransactionSpec) => void) | null } = { path: null, dispatch: null }

export function bindView(path: string | null, dispatch: ((spec: TransactionSpec) => void) | null) {
  bridge.path = path
  bridge.dispatch = dispatch
}

export function activeTab(): Tab | undefined {
  return workspace.tabs.find((t) => t.path === workspace.active)
}

export function resetFor(pack: string | null) {
  if (workspace.pack === pack) return
  workspace.pack = pack
  workspace.tabs = []
  workspace.active = null
  closed.length = 0
  treeState.children.clear()
  treeState.expanded.clear()
  treeUi.focused = null
  treeUi.status = null
  resetProbejs()
  resetKubejsContext()
}

function tab(path: string) {
  return workspace.tabs.find((t) => t.path === path)
}

export function updateDirty(path: string, state: EditorState) {
  const current = tab(path)
  if (!current) return
  current.state = state
  current.dirty = state.doc.length !== current.saved.length || state.doc.toString() !== current.saved
  if (current.dirty) current.preview = false
}

async function editorState(path: string, text: string, { readOnly = false, simplified = false } = {}) {
  const { createState } = await import('./editor')
  return createState(
    path,
    text,
    (state) => updateDirty(path, state),
    () => save(path),
    (problems) => {
      const current = tab(path)
      if (current) current.problems = problems
    },
    {
      readOnly,
      simplified,
      onBlocked: () => workspace.blocked++,
      onLintPaused: () => {
        const current = tab(path)
        if (current) current.lintPaused = true
      },
    },
  )
}

function stateOptions(content: { archived: boolean; compressed: boolean; large: boolean }) {
  return { readOnly: content.archived || content.compressed, simplified: content.large }
}

export function pin(path: string) {
  const current = tab(path)
  if (current) current.preview = false
}

// The log viewer reads logs itself, a piece at a time, so a log tab never loads its text here.
function logContent(path: string): FileContent {
  return { text: null, size: 0, unreadable: null, crlf: false, bom: false, archived: outerArchive(path) !== path, compressed: isGzip(path), large: false }
}

// Like VS Code, a preview tab shows a file until you open another one in its place. Editing
// the file or opening it with pinned set keeps it open.
export async function open(path: string, { pinned = true } = {}) {
  if (!tab(path)) {
    const content = isLog(path) ? logContent(path) : await api.readFile(path)
    const text = content.text ?? ''
    const state = content.text === null ? null : await editorState(path, text, stateOptions(content))
    // A double-click opens the same file three times at once, so another call may have
    // added the tab while we were reading.
    if (!tab(path)) addTab(path, content, text, state, pinned)
  }
  if (pinned) pin(path)
  workspace.active = path
}

function addTab(path: string, content: FileContent, text: string, state: EditorState | null, pinned: boolean) {
  const opened: Tab = {
    path,
    saved: text,
    dirty: false,
    crlf: content.crlf,
    bom: content.bom,
    unreadable: content.unreadable,
    conflict: null,
    disk: null,
    problems: { errors: 0, warnings: 0 },
    preview: !pinned,
    archived: content.archived,
    compressed: content.compressed,
    large: content.large,
    lintPaused: false,
    state,
  }
  const replaced = pinned ? -1 : workspace.tabs.findIndex((t) => t.preview)
  if (replaced >= 0) workspace.tabs[replaced] = opened
  else workspace.tabs.push(opened)
}

/** Shows read-only text in a tab that has no text of its own, like a decompiled class. */
export async function showSource(path: string, text: string) {
  const current = tab(path)
  if (!current || current.state) return
  const state = await editorState(path, text, { readOnly: true })
  Object.assign(current, { saved: text, dirty: false, unreadable: null, state })
}

/** Opens a file over the size you set to ask about, in the simplified editor. */
export async function openSimplified(path: string) {
  const current = tab(path)
  if (!current || current.state) return
  const content = await api.readFile(path, true)
  if (content.text === null) {
    current.unreadable = content.unreadable
    return
  }
  Object.assign(current, {
    saved: content.text,
    dirty: false,
    crlf: content.crlf,
    bom: content.bom,
    unreadable: null,
    lintPaused: false,
    state: await editorState(path, content.text, stateOptions(content)),
  })
}

export async function save(path = workspace.active) {
  const current = path ? tab(path) : undefined
  if (!current?.state || !current.dirty) return
  const text = current.state.doc.toString()
  try {
    await api.writeFile(current.path, text, current.crlf, current.bom)
    current.saved = text
    current.dirty = false
    current.conflict = null
    current.disk = null
    workspace.error = null
  } catch (error) {
    workspace.error = `Couldn't save ${current.path}: ${error}`
  }
}

export async function reload(path: string) {
  const current = tab(path)
  if (!current || isLog(path)) return
  const content = await api.readFile(path, current.large)
  const text = content.text ?? ''
  Object.assign(current, {
    saved: text,
    dirty: false,
    crlf: content.crlf,
    bom: content.bom,
    unreadable: content.unreadable,
    conflict: null,
    disk: null,
    problems: { errors: 0, warnings: 0 },
    large: content.large,
    lintPaused: false,
    state: content.text === null ? null : await editorState(path, text, stateOptions(content)),
  })
}

export function close(path: string, { remember = true } = {}) {
  const index = workspace.tabs.findIndex((t) => t.path === path)
  if (index < 0) return
  workspace.tabs.splice(index, 1)
  if (workspace.active === path) workspace.active = workspace.tabs[Math.min(index, workspace.tabs.length - 1)]?.path ?? null
  if (!remember) return
  closed.push(path)
  if (closed.length > MAX_CLOSED) closed.shift()
}

export function canReopen() {
  return closed.length > 0
}

export async function reopenClosed() {
  while (closed.length) {
    const path = closed.pop()!
    if (tab(path)) continue
    try {
      await open(path)
      return
    } catch {
      // The file may have been deleted since you closed it, so we try the next one.
    }
  }
}

// The editor state's save and change handlers capture the tab's path, so a moved tab needs a
// fresh state. Rebuilding it keeps unsaved text but drops undo history.
export async function moved(pairs: [string, string][]) {
  const destination = (path: string) => {
    for (const [from, to] of pairs) {
      if (path === from) return to
      if (path.startsWith(`${from}/`)) return to + path.slice(from.length)
    }
    return null
  }
  for (const current of [...workspace.tabs]) {
    const to = destination(current.path)
    if (!to) continue
    const state = current.state ? await editorState(to, current.state.doc.toString(), { readOnly: current.archived || current.compressed, simplified: current.large }) : null
    const wasActive = workspace.active === current.path
    current.path = to
    current.state = state
    if (state) current.dirty = state.doc.toString() !== current.saved
    if (wasActive) workspace.active = to
  }
  for (const [index, path] of closed.entries()) closed[index] = destination(path) ?? path
}

export function dirtyCount() {
  return workspace.tabs.filter((t) => t.dirty).length
}

function replacement(current: string, next: string): TransactionSpec {
  let start = 0
  while (start < current.length && start < next.length && current[start] === next[start]) start++
  let end = 0
  while (end < current.length - start && end < next.length - start && current[current.length - 1 - end] === next[next.length - 1 - end]) end++
  return { changes: { from: start, to: current.length - end, insert: next.slice(start, next.length - end) } }
}

function applyDiskText(current: Tab, text: string) {
  if (!current.state) return
  const spec = replacement(current.state.doc.toString(), text)
  current.saved = text
  current.conflict = null
  current.disk = null
  if (bridge.path === current.path && bridge.dispatch) {
    bridge.dispatch(spec)
  } else {
    current.state = current.state.update(spec).state
    current.dirty = false
  }
}

export function useDiskVersion(path: string) {
  const current = tab(path)
  if (current?.disk !== null && current?.disk !== undefined) applyDiskText(current, current.disk)
}

export function keepMine(path: string) {
  const current = tab(path)
  if (!current?.state) return
  if (current.disk !== null) current.saved = current.disk
  current.conflict = null
  current.disk = null
  current.dirty = current.state.doc.toString() !== current.saved
}

function parentDir(path: string) {
  const index = path.lastIndexOf('/')
  return index < 0 ? '' : path.slice(0, index)
}

export async function onDiskChange(paths: string[]) {
  fs.dirs = [...new Set(paths.flatMap((p) => [parentDir(p), p]))]
  fs.seq++
  for (const current of workspace.tabs.filter((t) => paths.includes(t.path) && t.state)) {
    let text: string | null
    try {
      text = (await api.readFile(current.path, current.large)).text
    } catch {
      current.conflict = 'deleted'
      continue
    }
    if (current.conflict === 'deleted') current.conflict = null
    if (text === null || text === current.saved) continue
    if (current.dirty) {
      current.conflict = 'changed'
      current.disk = text
    } else {
      applyDiskText(current, text)
    }
  }
}

let watching: Promise<unknown> | null = null

export function startWatching() {
  watching ??= listen<{ paths: string[] }>('fs-change', ({ payload }) => onDiskChange(payload.paths))
}
