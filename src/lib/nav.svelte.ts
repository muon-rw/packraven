export const nav = $state({ view: null as 'changes' | 'test' | 'mods' | 'git' | 'files' | 'releases' | 'settings' | 'pack' | null, diffPath: null as string | null, filePath: null as string | null, fileLine: null as number | null, textSearch: null as 'find' | 'replace' | null, editVersion: false, anchor: null as string | null })

// A view loads its data after it mounts, so we look for the section a link points at for a moment.
export function revealAnchor(id: string, frames = 120) {
  const target = document.getElementById(id)
  if (!target) {
    if (frames > 0) requestAnimationFrame(() => revealAnchor(id, frames - 1))
    return
  }
  const block = target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement ? 'center' : 'start'
  target.scrollIntoView({ block })
  target.focus({ preventScroll: true })
}

export const serverView = $state({ tab: 'mods' as 'mods' | 'properties' | 'startup', filter: 'excluded' as 'excluded' | 'overridden' | 'all' })

export const packSettingsView = $state({ tab: 'publishing' as 'publishing' | 'test' | 'discord' })

export const appSettingsView = $state({ tab: 'general' as 'general' | 'java' | 'accounts' })

export function showDiff(path: string) {
  nav.diffPath = path
  nav.view = 'git'
}

export function showInFiles(path: string, line?: number) {
  nav.filePath = path
  nav.fileLine = line ?? null
  nav.view = 'files'
}
