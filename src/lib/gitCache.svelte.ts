import type { GitCommit, GitDiff, GitStatus } from './api'

// We keep the last confirmed Git state for the active pack, so switching back to Git shows the
// file list, recent commits, and previously opened diffs right away while refreshing.
export const gitCache = $state({
  pack: null as string | null,
  status: null as GitStatus | null,
  commits: [] as GitCommit[],
})

const MAX_DIFF_ENTRIES = 120
const MAX_DIFF_CHARS = 16_000_000

const diffEntries = new Map<string, GitDiff>()
let diffChars = 0

export const gitDiffCache = {
  get(key: string): GitDiff | undefined {
    const found = diffEntries.get(key)
    if (!found) return undefined
    diffEntries.delete(key)
    diffEntries.set(key, found)
    return found
  },
  set(key: string, diff: GitDiff) {
    const existing = diffEntries.get(key)
    if (existing) {
      diffChars -= existing.text.length
      diffEntries.delete(key)
    }
    if (diff.text.length > MAX_DIFF_CHARS) return
    diffEntries.set(key, diff)
    diffChars += diff.text.length
    while (diffEntries.size > MAX_DIFF_ENTRIES || diffChars > MAX_DIFF_CHARS) {
      const oldest = diffEntries.keys().next().value
      if (oldest === undefined) break
      const removed = diffEntries.get(oldest)
      if (removed) diffChars -= removed.text.length
      diffEntries.delete(oldest)
    }
  },
  clear() {
    diffEntries.clear()
    diffChars = 0
  },
}
