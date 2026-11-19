import { getContext, setContext } from 'svelte'
import { appSettingsView, packSettingsView, serverView } from './nav.svelte'
import { StoredSplit } from './storedSplit.svelte'

export const views = ['changes', 'test', 'mods', 'server', 'files', 'git', 'publish', 'releases', 'pack', 'settings', 'add'] as const
export type View = (typeof views)[number]

export const titles: Record<View, string> = {
  changes: 'Changes',
  test: 'Run',
  mods: 'Mods',
  server: 'Server',
  files: 'Files',
  git: 'Git',
  publish: 'Publish',
  releases: 'Releases',
  pack: 'Pack settings',
  settings: 'App settings',
  add: 'Add pack',
}

// Each view shows in at most one pane, since views keep their state (open files, the Server tab,
// element ids) in modules shared by every instance.
export const layout = $state({ panes: ['mods'] as View[], focused: 0 })

// We store the left pane's share of the width, so the split keeps its proportions as the window resizes.
export const paneSplit = new StoredSplit('packraven.paneSplit')

let previous: View | null = null
// Add pack takes the whole work area, so we set the split aside and bring it back when you leave.
let parked: { panes: View[]; focused: number } | null = null

export function focusedView() {
  return layout.panes[layout.focused]
}

export function isSplit() {
  return layout.panes.length > 1
}

export function only(view: View) {
  if (view === 'add' && isSplit()) parked = { panes: [...layout.panes], focused: layout.focused }
  else if (view !== 'add') parked = null
  layout.panes = [view]
  layout.focused = 0
}

export function show(view: View, pane?: number) {
  if (view === 'add') return only(view)
  if (focusedView() === 'add' && parked) {
    layout.panes = parked.panes
    layout.focused = parked.focused
    parked = null
    pane = undefined
  }
  const at = layout.panes.indexOf(view)
  if (at >= 0) {
    layout.focused = at
    return
  }
  const target = pane ?? layout.focused
  previous = layout.panes[target]
  layout.panes[target] = view
  layout.focused = target
}

// Links inside a view open in the other pane of a split, so you keep what you were reading.
export function showFrom(view: View, pane: number) {
  show(view, isSplit() ? 1 - pane : pane)
}

export function openBeside(view: View) {
  const current = focusedView()
  if (view === 'add' || current === 'add') return show(view)
  const at = layout.panes.indexOf(view)
  if (at >= 0) {
    layout.focused = at
    return
  }
  if (isSplit()) {
    const other = 1 - layout.focused
    layout.panes[other] = view
    layout.focused = other
  } else {
    layout.panes.push(view)
    layout.focused = 1
  }
}

export function closePane(pane: number) {
  if (!isSplit()) return
  previous = layout.panes[pane]
  layout.panes.splice(pane, 1)
  layout.focused = 0
}

export function swapPanes() {
  layout.panes.reverse()
  layout.focused = 1 - layout.focused
}

// Dragging a rail item onto the left or right half of the work area puts it on that side,
// splitting when only one view was open or swapping when the item was already on the other side.
export function placeInPane(view: View, side: 0 | 1) {
  const current = focusedView()
  if (view === 'add' || current === 'add') return show(view)
  const at = layout.panes.indexOf(view)
  if (isSplit()) {
    if (at === side) layout.focused = side
    else if (at >= 0) {
      layout.panes.reverse()
      layout.focused = side
    } else {
      previous = layout.panes[side]
      layout.panes[side] = view
      layout.focused = side
    }
  } else if (at === 0) {
    openBeside(splitCompanion())
    if (side === 1) layout.panes.reverse()
    layout.focused = side
  } else if (side === 0) {
    layout.panes.unshift(view)
    layout.focused = 0
  } else {
    layout.panes.push(view)
    layout.focused = 1
  }
}

// We split with the view you were on before, or Files when there's none to go back to.
export function splitCompanion(): View {
  const current = focusedView()
  return previous && previous !== current && previous !== 'add' ? previous : current === 'files' ? 'git' : 'files'
}

export function toggleSplit() {
  if (isSplit()) return closePane(1 - layout.focused)
  if (focusedView() === 'add') return
  openBeside(splitCompanion())
  layout.focused = 0
}

const PANE = Symbol('pane')

export function providePane(focused: () => boolean) {
  setContext(PANE, focused)
}

// Views with window-wide shortcuts check this, so Ctrl+S or Ctrl+F only reaches the focused pane.
export function paneFocus(): () => boolean {
  return getContext<(() => boolean) | undefined>(PANE) ?? (() => true)
}

type HistoryTabs = {
  server?: typeof serverView.tab
  pack?: typeof packSettingsView.tab
  settings?: typeof appSettingsView.tab
}

type HistoryEntry = {
  panes: View[]
  focused: number
  tabs: HistoryTabs
}

const HISTORY_LIMIT = 50
const navHistory = $state({ entries: [] as HistoryEntry[], at: -1 })

function snapshot(): HistoryEntry {
  const panes = [...layout.panes]
  const tabs: HistoryTabs = {}
  if (panes.includes('server')) tabs.server = serverView.tab
  if (panes.includes('pack')) tabs.pack = packSettingsView.tab
  if (panes.includes('settings')) tabs.settings = appSettingsView.tab
  return { panes, focused: layout.focused, tabs }
}

function sameEntry(a: HistoryEntry, b: HistoryEntry): boolean {
  if (a.panes.length !== b.panes.length) return false
  for (let i = 0; i < a.panes.length; i++) if (a.panes[i] !== b.panes[i]) return false
  return a.tabs.server === b.tabs.server && a.tabs.pack === b.tabs.pack && a.tabs.settings === b.tabs.settings
}

function restore(entry: HistoryEntry) {
  previous = layout.panes[layout.focused] ?? previous
  parked = null
  layout.panes = [...entry.panes]
  layout.focused = Math.min(entry.focused, entry.panes.length - 1)
  if (entry.tabs.server !== undefined) serverView.tab = entry.tabs.server
  if (entry.tabs.pack !== undefined) packSettingsView.tab = entry.tabs.pack
  if (entry.tabs.settings !== undefined) appSettingsView.tab = entry.tabs.settings
}

export function resetHistory() {
  navHistory.entries = [snapshot()]
  navHistory.at = 0
}

export function recordHistory() {
  const next = snapshot()
  if (navHistory.at >= 0 && sameEntry(navHistory.entries[navHistory.at], next)) {
    navHistory.entries[navHistory.at].focused = next.focused
    return
  }
  const trimmed = navHistory.entries.slice(0, navHistory.at + 1)
  trimmed.push(next)
  if (trimmed.length > HISTORY_LIMIT) trimmed.shift()
  navHistory.entries = trimmed
  navHistory.at = trimmed.length - 1
}

function findHistory(step: -1 | 1, canOpen?: (view: View) => boolean): number {
  for (let i = navHistory.at + step; i >= 0 && i < navHistory.entries.length; i += step) {
    if (!canOpen || navHistory.entries[i].panes.every(canOpen)) return i
  }
  return -1
}

export function canGoBack(canOpen?: (view: View) => boolean): boolean {
  return findHistory(-1, canOpen) >= 0
}

export function canGoForward(canOpen?: (view: View) => boolean): boolean {
  return findHistory(1, canOpen) >= 0
}

export function goBack(canOpen?: (view: View) => boolean): boolean {
  const target = findHistory(-1, canOpen)
  if (target < 0) return false
  navHistory.at = target
  restore(navHistory.entries[target])
  return true
}

export function goForward(canOpen?: (view: View) => boolean): boolean {
  const target = findHistory(1, canOpen)
  if (target < 0) return false
  navHistory.at = target
  restore(navHistory.entries[target])
  return true
}
