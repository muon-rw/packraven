import { api, type Overview } from './api'
import { job, testState } from './jobs.svelte'
import { browse, modUpdates } from './modUpdates.svelte'
import { serverView } from './nav.svelte'
import { focusedView } from './panes.svelte'
import { viewerFor } from './viewers'
import { activeTab } from './workspace.svelte'

export type ModKind = 'mods' | 'resourcepacks' | 'shaderpacks' | 'datapacks'

export const presenceContext = $state({
  modName: null as string | null,
  modKind: 'mods' as ModKind,
  modReviewing: false,
  browseModName: null as string | null,
  gitFile: null as string | null,
  gitConflicts: false,
})

export interface PresenceActivity {
  specific: string | null
  generic: string | null
}

function same(text: string): PresenceActivity {
  return { specific: text, generic: text }
}

function basename(path: string): string {
  const clean = path.replace(/\\/g, '/').replace(/\/+$/, '')
  const slash = clean.lastIndexOf('/')
  return slash >= 0 ? clean.slice(slash + 1) : clean
}

function isCrashReport(path: string): boolean {
  return /^crash-reports\//i.test(path.replace(/\\/g, '/'))
}

function kindLabel(kind: ModKind): string {
  switch (kind) {
    case 'resourcepacks':
      return 'resource packs'
    case 'shaderpacks':
      return 'shader packs'
    case 'datapacks':
      return 'data packs'
    default:
      return 'mods'
  }
}

export function describePresence(overview: Overview | null): PresenceActivity {
  if (job('publish').phase === 'running') {
    return {
      specific: overview?.version ? `Publishing v${overview.version}` : 'Publishing a release',
      generic: 'Publishing a release',
    }
  }
  const launchingClient = (!overview?.clientRunning && job('client').phase === 'running') || (!overview?.secondClientRunning && job('second-client').phase === 'running')
  if (job('test').phase === 'running' || launchingClient) {
    return same('Launching the game')
  }
  if (job('server-pack').phase === 'running') {
    return same('Building a server pack')
  }
  if (job('client-export').phase === 'running' || job('modrinth-export').phase === 'running') {
    return same('Exporting the pack')
  }

  switch (focusedView()) {
    case 'files': {
      const tab = activeTab()
      if (!tab) return same('Browsing files')
      const name = basename(tab.path)
      const viewer = viewerFor(tab.path)
      if (viewer === 'log' || isCrashReport(tab.path)) {
        return { specific: `Digging through ${name}`, generic: 'Digging through the logs' }
      }
      if (tab.archived || viewer === 'class') {
        return { specific: `Inspecting ${name}`, generic: 'Inspecting mod files' }
      }
      if (viewer === 'image' || viewer === 'video' || viewer === 'audio' || viewer === 'nbt') {
        return { specific: `Viewing ${name}`, generic: 'Browsing files' }
      }
      return { specific: `Editing ${name}`, generic: 'Editing a file' }
    }
    case 'mods': {
      if (job('mods').phase === 'running') {
        const active = modUpdates.changes.filter((change) => change.stage !== 'finished')
        if (active.length) {
          const names = active.flatMap((change) => change.names)
          const verb = active.every((change) => change.verb === 'install') ? 'Installing' : 'Updating'
          if (names.length === 1) return { specific: `${verb} ${names[0]}`, generic: `${verb} mods` }
          return same(`${verb} ${names.length} mods`)
        }
        return same('Updating mods')
      }
      if (browse.platform) {
        const site = browse.platform === 'curseforge' ? 'CurseForge' : 'Modrinth'
        if (presenceContext.browseModName) {
          return { specific: `Looking at ${presenceContext.browseModName}`, generic: `Browsing ${site}` }
        }
        return same(`Browsing ${site}`)
      }
      if (presenceContext.modReviewing) return same('Reviewing mod updates')
      const label = kindLabel(presenceContext.modKind)
      if (presenceContext.modName) {
        return { specific: `Looking at ${presenceContext.modName}`, generic: `Looking at ${label}` }
      }
      return same(`Looking at ${label}`)
    }
    case 'changes':
      return same('Writing a changelog')
    case 'test': {
      if (overview?.serverRunning && !testState.serverReady) return same('Starting the test server')
      if (overview?.clientRunning || overview?.secondClientRunning) return same('Playtesting the pack')
      if (overview?.serverRunning) return same('Running a test server')
      const hasLogs =
        job('test').lines.length > 0 ||
        job('server').lines.length > 0 ||
        job('client').lines.length > 0 ||
        job('second-client').lines.length > 0
      return same(hasLogs ? 'Digging through the logs' : 'Getting ready to launch')
    }
    case 'git': {
      if (job('git').phase === 'running') return same('Syncing with Git')
      if (presenceContext.gitConflicts) return same('Resolving mod conflicts')
      if (presenceContext.gitFile) {
        return { specific: `Reviewing ${basename(presenceContext.gitFile)}`, generic: 'Reviewing Git changes' }
      }
      return same('Reviewing Git changes')
    }
    case 'server': {
      if (serverView.tab === 'properties') return { specific: 'Editing server.properties', generic: 'Configuring the server' }
      if (serverView.tab === 'startup') return same('Configuring server startup')
      return same('Configuring server mods')
    }
    case 'publish':
      return {
        specific: overview?.version ? `Preparing v${overview.version}` : 'Preparing a release',
        generic: 'Preparing a release',
      }
    case 'releases':
      return same('Looking at releases')
    case 'pack':
      return same('Editing pack settings')
    case 'settings':
      return same('Configuring packraven')
    case 'add':
      return same('Setting up a pack')
  }
}

const DEBOUNCE_MS = 600
let lastSent: PresenceActivity | null = null
let pending: PresenceActivity | null = null
let timer: ReturnType<typeof setTimeout> | null = null

function sendNow(activity: PresenceActivity) {
  if (lastSent && lastSent.specific === activity.specific && lastSent.generic === activity.generic) return
  lastSent = activity
  api.setDiscordActivity(activity.specific, activity.generic).catch(() => {})
}

export function syncPresence(activity: PresenceActivity) {
  if (lastSent === null) {
    sendNow(activity)
    return
  }
  if (lastSent.specific === activity.specific && lastSent.generic === activity.generic) {
    if (timer) {
      clearTimeout(timer)
      timer = null
      pending = null
    }
    return
  }
  pending = activity
  if (timer) clearTimeout(timer)
  timer = setTimeout(() => {
    timer = null
    const next = pending
    pending = null
    if (next) sendNow(next)
  }, DEBOUNCE_MS)
}

export function cancelPendingPresence() {
  if (timer) {
    clearTimeout(timer)
    timer = null
  }
  pending = null
}
