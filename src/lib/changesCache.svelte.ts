import type { ModDiff } from './api'

// We keep the last confirmed release notes and mod diff for the active pack, so switching back to
// Changes renders right away while refreshing in the background.
export const changesCache = $state({
  pack: null as string | null,
  notes: '',
  savedNotes: '',
  diff: null as ModDiff | null,
  keys: null as { upload: boolean; modrinth: boolean } | null,
})
