import { untrack } from 'svelte'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

export type JobName = 'client-export' | 'modrinth-export' | 'server-pack' | 'test' | 'server' | 'client' | 'publish' | 'setup' | 'git' | 'restore' | 'mods'
export type JobPhase = 'idle' | 'running' | 'done' | 'failed'

export interface LogLine {
  id: number
  stream: string
  text: string
}

class JobRecord {
  phase = $state<JobPhase>('idle')
  message = $state<string | null>(null)
  lines = $state.raw<LogLine[]>([])
  // The phase stays 'done' or 'failed' until the next run starts, so effects that react to a run
  // ending compare this count instead.
  finishedRuns = $state(0)
  // The pack that was open when the run started, so another pack's views don't show its result.
  pack: string | null = null
}

export function onJobEnd(name: JobName, run: (phase: JobPhase) => void) {
  let seen = job(name).finishedRuns
  $effect(() => {
    const record = job(name)
    if (record.finishedRuns === seen) return
    seen = record.finishedRuns
    const phase = record.phase
    untrack(() => run(phase))
  })
}

export type JobItemState = 'started' | 'finished' | 'failed'
type JobItemListener = (job: string, id: number, state: JobItemState) => void
const itemListeners: JobItemListener[] = []

// Queued jobs report each item as it starts and ends, keyed by the id their command returned.
export function onJobItem(listener: JobItemListener) {
  itemListeners.push(listener)
}

const MAX_LINES = 4000
const FLUSH_MS = 50
let nextId = 0

export const jobs: Record<string, JobRecord> = $state({})
// player is the name the backend confirmed, and serverOnline the mode the test server started in.
export const testState = $state({ serverReady: false, players: [] as string[], player: null as string | null, serverOnline: null as boolean | null, clientJoins: false })
export const serverPack = $state({ changedSinceBuild: false, changedSinceTest: false })
export const testOptions = $state({ recopyAll: false, joinServer: false })

const serverStopListeners: (() => void)[] = []

export function onServerStopped(listener: () => void) {
  serverStopListeners.push(listener)
}

const idle = new JobRecord()
// Setup adds a pack and then switches to it, so its result belongs to no one pack.
const unscoped: ReadonlySet<string> = new Set<JobName>(['setup'])
const scope = $state({ pack: null as string | null })

export function setJobPack(pack: string | null) {
  scope.pack = pack
}
const pending = new Map<string, LogLine[]>()
let flushTimer: ReturnType<typeof setTimeout> | null = null

function record(job: string): JobRecord {
  jobs[job] ??= new JobRecord()
  return jobs[job]
}

export function job(name: JobName): JobRecord {
  const entry = jobs[name]
  if (!entry) return idle
  return unscoped.has(name) || entry.pack === null || entry.pack === scope.pack ? entry : idle
}

export function clearLog(...names: JobName[]) {
  for (const name of names) {
    pending.delete(name)
    if (jobs[name]) jobs[name].lines = []
  }
}

function flushLogs() {
  if (flushTimer) clearTimeout(flushTimer)
  flushTimer = null
  for (const [name, batch] of pending) {
    const entry = record(name)
    const merged = entry.lines.concat(batch)
    entry.lines = merged.length > MAX_LINES ? merged.slice(-MAX_LINES) : merged
  }
  pending.clear()
}

function queueLine(job: string, line: LogLine) {
  const batch = pending.get(job)
  if (batch) {
    batch.push(line)
    if (batch.length > MAX_LINES) batch.splice(0, batch.length - MAX_LINES)
  } else {
    pending.set(job, [line])
  }
  flushTimer ??= setTimeout(flushLogs, FLUSH_MS)
}

let listeners: Promise<UnlistenFn[]> | null = null

if (import.meta.hot) {
  import.meta.hot.dispose(async () => (await listeners)?.forEach((unlisten) => unlisten()))
}

export function startListening(): Promise<UnlistenFn[]> {
  listeners ??= register()
  return listeners
}

function register(): Promise<UnlistenFn[]> {
  return Promise.all([
  listen<{ job: string; stream: string; text: string }>('job-log', ({ payload }) => {
    queueLine(payload.job, { id: nextId++, stream: payload.stream, text: payload.text })
  }),
  listen<{ job: string; state: JobPhase; message: string | null }>('job-state', ({ payload }) => {
    const entry = record(payload.job)
    if (payload.state === 'running') {
      pending.delete(payload.job)
      entry.lines = []
      entry.pack = scope.pack
    } else {
      flushLogs()
    }
    entry.phase = payload.state
    entry.message = payload.message
    if (payload.state !== 'running') entry.finishedRuns++
  }),
  listen<{ job: string; id: number; state: JobItemState }>('job-item', ({ payload }) => {
    for (const listener of itemListeners) listener(payload.job, payload.id, payload.state)
  }),
  listen<{ kind: string; detail: string }>('test-event', ({ payload }) => {
    if (payload.kind === 'server-started') {
      testState.serverReady = false
      testState.players = []
    } else if (payload.kind === 'server-ready') {
      testState.serverReady = true
    } else if (payload.kind === 'server-stopped') {
      testState.serverReady = false
      testState.players = []
      testState.serverOnline = null
      for (const listener of serverStopListeners) listener()
    } else if (payload.kind === 'player-resolved') {
      testState.player = payload.detail
    } else if (payload.kind === 'server-online') {
      testState.serverOnline = payload.detail === 'true'
    } else if (payload.kind === 'player-joined' && !testState.players.includes(payload.detail)) {
      testState.players.push(payload.detail)
    }
  }),
  ])
}
