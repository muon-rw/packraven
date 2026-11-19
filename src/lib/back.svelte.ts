export type BackSource = 'escape' | 'back'

export const BACK_PRIORITY = {
  overlay: 100,
  dialog: 90,
  prompt: 70,
  detail: 50,
  selection: 40,
  filter: 30,
  mode: 20,
} as const

type BackEntry = {
  id: number
  priority: number
  run: (source: BackSource) => boolean
}

let nextId = 0
const entries: BackEntry[] = []

export function registerBack(priority: number, run: (source: BackSource) => boolean): () => void {
  const entry: BackEntry = { id: ++nextId, priority, run }
  entries.push(entry)
  return () => {
    const at = entries.indexOf(entry)
    if (at >= 0) entries.splice(at, 1)
  }
}

export function useBack(priority: number, run: (source: BackSource) => boolean) {
  $effect(() => registerBack(priority, run))
}

function openDialog(): HTMLDialogElement | null {
  const dialogs = document.querySelectorAll<HTMLDialogElement>('dialog[open]')
  return dialogs.length ? dialogs[dialogs.length - 1] : null
}

export function hasOpenModal(): boolean {
  return !!document.querySelector('dialog[open], [role="dialog"][aria-modal="true"]')
}

// Returns 'handled' when a local layer consumed the action, 'native' when a native control
// (modal dialog or focused non-empty search input) should handle Escape itself, or 'none' when
// nothing local applies.
export function runBack(source: BackSource, target?: EventTarget | null): 'handled' | 'native' | 'none' {
  const ordered = [...entries].sort((a, b) => b.priority - a.priority || b.id - a.id)

  for (const entry of ordered) {
    if (entry.priority <= BACK_PRIORITY.dialog) break
    if (entry.run(source)) return 'handled'
  }

  const dialog = openDialog()
  if (dialog) {
    if (source === 'escape') return 'native'
    const cancel = new Event('cancel', { cancelable: true })
    if (dialog.dispatchEvent(cancel)) dialog.close()
    return 'handled'
  }

  for (const entry of ordered) {
    if (entry.priority > BACK_PRIORITY.dialog || entry.priority < BACK_PRIORITY.prompt) continue
    if (entry.run(source)) return 'handled'
  }

  if (source === 'escape' && target instanceof HTMLInputElement && target.type === 'search' && target.value) {
    return 'native'
  }

  for (const entry of ordered) {
    if (entry.priority >= BACK_PRIORITY.prompt) continue
    if (entry.run(source)) return 'handled'
  }

  return 'none'
}
