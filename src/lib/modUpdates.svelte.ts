import { untrack } from 'svelte'
import { api, type AddonKind, type ModCheck, type ModFile, type Platform, type ReleaseType, type SearchSort } from './api'
import { job, onJobItem } from './jobs.svelte'

const stability: Record<ReleaseType, number> = { release: 1, beta: 2, alpha: 3 }

// We keep the last check for the session, so leaving the Mods view doesn't mean asking CurseForge again.
export const modUpdates = $state({
  pack: null as string | null,
  channel: 'release' as ReleaseType,
  // We replace the set on each change, since Svelte doesn't track changes inside a Set.
  locked: new Set<string>(),
  checks: null as Map<string, ModCheck> | null,
  checkedAt: 0,
  // Incremented on each check and pack switch, so an earlier check that finishes late is ignored.
  checkSerial: 0,
  // Changes you ask for while one runs join that run. We keep each until the run ends, since we
  // only reload the mod list then.
  changes: [] as QueuedChange[],
  // What became of the changes you tried to cancel, kept until you ask for another change.
  cancelNotes: [] as string[],
  // The change whose Cancel you're pointing at, so every row it covers can show what you'd cancel.
  pointing: null as number | null,
  // Brief confirmation after copying a link, Maven snippet, or ID.
  copyNote: null as string | null,
})

let copyTimer: ReturnType<typeof setTimeout> | undefined

export function flashCopyNote(note: string) {
  clearTimeout(copyTimer)
  modUpdates.copyNote = note
  copyTimer = setTimeout(() => {
    if (modUpdates.copyNote === note) modUpdates.copyNote = null
  }, 3000)
}

export function projectKey(key: string): string {
  return `project:${key}`
}

export interface QueuedChange {
  serial: number
  // Updates go in by the path they replace, and installs by projectKey.
  keys: string[]
  names: string[]
  verb: 'update' | 'install'
  // Null until change_mods returns it.
  id: number | null
  stage: 'queued' | 'started' | 'finished'
  cancelOnceQueued: boolean
}

let nextSerial = 0

export function startChange(keys: string[], names: string[], verb: QueuedChange['verb']): number {
  modUpdates.cancelNotes = []
  const serial = nextSerial++
  modUpdates.changes.push({ serial, keys, names, verb, id: null, stage: 'queued', cancelOnceQueued: false })
  return serial
}

// A change can start before change_mods returns its id, so we hold events for ids we don't know yet.
const earlyStages = new Map<number, QueuedChange['stage']>()

export function changeQueued(serial: number, id: number) {
  const change = modUpdates.changes.find((c) => c.serial === serial)
  if (!change) return
  change.id = id
  change.stage = earlyStages.get(id) ?? change.stage
  earlyStages.delete(id)
  if (change.cancelOnceQueued) cancel(change, change.names[0])
}

export function dropChange(serial: number) {
  modUpdates.changes = modUpdates.changes.filter((c) => c.serial !== serial)
}

export function changeFor(key: string): QueuedChange | undefined {
  return modUpdates.changes.find((c) => c.keys.includes(key))
}

export function downloadingCount(): number {
  return modUpdates.changes.filter((c) => c.stage !== 'finished').reduce((sum, c) => sum + c.keys.length, 0)
}

function list(names: string[]) {
  if (names.length > 4) return `${names.slice(0, 3).join(', ')} and ${names.length - 3} more`
  return names.length < 3 ? names.join(' and ') : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`
}

// An install leads with the mod you asked for, even from the row of a dependency it brought.
function describe(change: QueuedChange, name: string) {
  const lead = change.verb === 'install' ? change.names[0] : name
  const others = change.names.length - 1
  return `${change.verb === 'update' ? 'updating' : 'installing'} ${lead}${others ? ` and ${others} other ${others === 1 ? 'mod' : 'mods'}` : ''}`
}

export function cancelMenuLabel(change: QueuedChange) {
  return `Cancel ${change.verb}${change.names.length > 1 ? ` (${change.names.length} mods)` : ''}`
}

export function queuedButton(change: QueuedChange, name: string) {
  const [doing, done] = change.verb === 'update' ? ['Updating', 'Updated'] : ['Installing', 'Installed']
  if (change.stage === 'finished') return { label: done, ariaLabel: `${done} ${name}`, title: 'packraven refreshes this list once the other changes finish', busy: false, cancels: false }
  if (change.stage === 'started') return { label: doing, ariaLabel: `${doing} ${name}`, title: `packraven is ${doing.toLowerCase()} it, so it's too late to cancel`, busy: true, cancels: false }
  const others = change.names.filter((n) => n !== name)
  const title = others.length ? `Cancel this ${change.verb}. It also cancels ${list(others)}.` : `Cancel this ${change.verb}`
  return { label: doing, ariaLabel: `Cancel ${doing.toLowerCase()} ${name}`, title, busy: true, cancels: true }
}

// Cancelling one key cancels the change it's part of, so every row it covers goes back too.
export async function cancelChange(key: string, name: string) {
  const change = changeFor(key)
  if (!change || change.stage !== 'queued') return
  // You can click before change_mods returns the id, so we cancel once it does.
  if (change.id === null) change.cancelOnceQueued = true
  else await cancel(change, name)
}

async function cancel(change: QueuedChange, name: string) {
  const cancelled = await api.cancelModChange(change.id!)
  modUpdates.cancelNotes.push(cancelled ? `Cancelled ${describe(change, name)}.` : `Too late to cancel ${describe(change, name)}.`)
  if (cancelled) dropChange(change.serial)
  // The backend only refuses once the change starts, and its event may still be on the way.
  else if (change.stage === 'queued') change.stage = 'started'
}

export function point(change: QueuedChange | undefined, on: boolean) {
  if (on && change?.stage === 'queued') modUpdates.pointing = change.serial
  else if (!on && modUpdates.pointing === change?.serial) modUpdates.pointing = null
}

onJobItem((name, id, state) => {
  if (name !== 'mods') return
  const stage = state === 'started' ? 'started' : 'finished'
  const change = modUpdates.changes.find((c) => c.id === id)
  if (change) change.stage = stage
  else earlyStages.set(id, stage)
})

$effect.root(() => {
  $effect(() => {
    if (!job('mods').finishedRuns) return
    untrack(() => {
      earlyStages.clear()
      modUpdates.changes = []
      modUpdates.pointing = null
    })
  })
})

// In a pack on Modrinth, an update only counts once Modrinth has it too, so updating never takes a
// mod off the Modrinth pack. CurseForge files Modrinth doesn't have yet still show in the versions list.
export function newestOnChannel(check: ModCheck | undefined): ModFile | undefined {
  return check?.newer.find((file) => stability[file.releaseType] <= stability[modUpdates.channel] && file.onModrinth !== false)
}

// The newest file on the channel that Modrinth doesn't have yet, when it's newer than the update we offer.
export function waitingOnModrinth(check: ModCheck | undefined): ModFile | undefined {
  const first = check?.newer.find((file) => stability[file.releaseType] <= stability[modUpdates.channel])
  return first?.onModrinth === false ? first : undefined
}

export function isLocked(key: string | null): boolean {
  return key !== null && modUpdates.locked.has(key)
}

export function availableUpdate(check: ModCheck | undefined): ModFile | undefined {
  return check && isLocked(check.key) ? undefined : newestOnChannel(check)
}

export function updateCount(pack: string | null): number {
  if (!modUpdates.checks || modUpdates.pack !== pack) return 0
  let count = 0
  for (const check of modUpdates.checks.values()) if (availableUpdate(check)) count++
  return count
}

// Null shows the installed list. A platform shows that platform's search, in place of the list.
export const browse = $state({
  platform: null as Platform | null,
  text: '',
  kind: 'mods' as AddonKind,
  category: null as string | null,
  sort: 'relevance' as SearchSort,
})
