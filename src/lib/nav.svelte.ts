export const nav = $state({ view: null as 'changes' | 'test' | 'mods' | 'git' | 'files' | 'releases' | 'settings' | 'pack' | null, diffPath: null as string | null, filePath: null as string | null, textSearch: null as 'find' | 'replace' | null, editVersion: false, anchor: null as string | null })

// A view loads its data after it mounts, so we look for the section a link points at for a moment.
export function revealAnchor(id: string, frames = 60) {
  const target = document.getElementById(id)
  if (!target) {
    if (frames > 0) requestAnimationFrame(() => revealAnchor(id, frames - 1))
    return
  }
  target.scrollIntoView({ block: 'start' })
  target.focus({ preventScroll: true })
}

export const serverView = $state({ tab: 'mods' as 'mods' | 'properties' | 'startup', filter: 'excluded' as 'excluded' | 'overridden' | 'all' })

export function showDiff(path: string) {
  nav.diffPath = path
  nav.view = 'git'
}

export function showInFiles(path: string) {
  nav.filePath = path
  nav.view = 'files'
}
