import type { Extension } from '@codemirror/state'
import { ViewPlugin, hoverTooltip, type EditorView, type Tooltip, type ViewUpdate } from '@codemirror/view'
import { autocompletion, completionStatus, startCompletion, type Completion, type CompletionContext, type CompletionResult } from '@codemirror/autocomplete'
import { api, errorText } from './api'

export type DatapackSchemaStatus = 'idle' | 'loading' | 'ready' | 'unavailable' | 'error'
export type CodeRegistrySource = 'vanillaOnly' | 'probeJs' | 'probeRaven'

export const datapack = $state({
  status: 'idle' as DatapackSchemaStatus,
  minecraft: null as string | null,
  loader: null as string | null,
  symbolCount: 0,
  registryEntries: 0,
  codeSource: 'vanillaOnly' as CodeRegistrySource,
  probeAvailable: false,
  loadMs: 0,
  error: null as string | null,
})

const RECHECK_AFTER_MS = 15_000

let loadPromise: Promise<boolean> | null = null
let lastFailedAt = 0

export function isDatapackJson(path: string): boolean {
  const norm = path.replace(/\\/g, '/')
  const lower = norm.toLowerCase()
  if (lower === 'pack.mcmeta' || lower.endsWith('/pack.mcmeta')) return true
  // Minecraft only loads `.json` data files, so `.jsonc` files get the plain JSON checks.
  if (!lower.endsWith('.json')) return false
  return /(?:^|\/)data\/[^/]+\/[^/]+\/.+\.json$/i.test(norm)
}

export function resetDatapack() {
  loadPromise = null
  datapack.status = 'idle'
  datapack.minecraft = null
  datapack.loader = null
  datapack.symbolCount = 0
  datapack.registryEntries = 0
  datapack.codeSource = 'vanillaOnly'
  datapack.probeAvailable = false
  datapack.loadMs = 0
  datapack.error = null
}

function failedRecently() {
  return (datapack.status === 'unavailable' || datapack.status === 'error') && Date.now() - lastFailedAt < RECHECK_AFTER_MS
}

export async function ensureDatapack(force = false): Promise<boolean> {
  if (!force && datapack.status === 'ready') return true
  if (!force && failedRecently()) return false
  if (!force && loadPromise) return loadPromise

  datapack.status = 'loading'
  datapack.error = null

  let run!: Promise<boolean>
  run = (async () => {
    try {
      const info = await api.datapackStatus(force)
      datapack.minecraft = info.minecraft
      datapack.loader = info.loader
      datapack.symbolCount = info.symbolCount
      datapack.registryEntries = info.registryEntries
      datapack.codeSource = info.codeSource
      datapack.probeAvailable = info.probeAvailable
      datapack.loadMs = info.loadMs
      datapack.error = info.error
      if (!info.available) {
        datapack.status = info.error ? 'error' : 'unavailable'
        lastFailedAt = Date.now()
        return false
      }
      datapack.status = 'ready'
      return true
    } catch (e) {
      datapack.status = 'error'
      datapack.error = errorText(e)
      lastFailedAt = Date.now()
      return false
    } finally {
      if (loadPromise === run) loadPromise = null
    }
  })()

  loadPromise = run
  return run
}

export async function refreshDatapack(): Promise<boolean> {
  return ensureDatapack(true)
}

function triggerCharAt(context: CompletionContext): string | undefined {
  if (context.pos <= 0) return undefined
  const ch = context.state.sliceDoc(context.pos - 1, context.pos)
  return ch === '"' || ch === ':' || ch === '/' || ch === '#' || ch === '{' || ch === '[' ? ch : undefined
}

function renderDocTooltip(signature: string, docs: string): HTMLElement {
  const box = document.createElement('div')
  box.className = 'cm-probejs-tip'
  if (signature) {
    const code = document.createElement('pre')
    code.className = 'cm-probejs-sig'
    code.textContent = signature
    box.appendChild(code)
  }
  if (docs) {
    const p = document.createElement('p')
    p.className = 'cm-probejs-doc'
    p.textContent = docs
    box.appendChild(p)
  }
  return box
}

function completionSource(path: string) {
  return async (context: CompletionContext): Promise<CompletionResult | null> => {
    const trigger = triggerCharAt(context)
    if (!context.explicit && !trigger) {
      const word = context.matchBefore(/[A-Za-z0-9_./:#-]+/)
      if (!word || word.from === word.to) return null
    }
    if (datapack.status !== 'ready') {
      const ok = await ensureDatapack()
      if (!ok) return null
    }
    const text = context.state.doc.toString()
    const res = await api.datapackComplete(path, text, context.pos).catch(() => null)
    if (context.aborted || !res || !res.items.length) return null

    // Accepting replaces the rest of the word or string after the cursor too, which stays the same length while you type.
    const suffix = Math.max(0, (res.replaceTo ?? res.to) - res.to)
    const options: Completion[] = res.items.map((item) => {
      const insert = item.insertText ?? item.label
      const customApply = suffix > 0 || item.cursorOffset != null || !!item.selectLen || item.consumeClosingQuote || item.retrigger
      return {
        label: item.label,
        type: item.kind,
        detail: item.detail ?? undefined,
        apply: customApply
          ? (view, _completion, from, to) => {
              let end = to + suffix
              if (item.consumeClosingQuote && view.state.sliceDoc(end, end + 1) === '"') {
                end += 1
              }
              const anchor = from + (item.cursorOffset ?? insert.length)
              view.dispatch({
                changes: { from, to: end, insert },
                // A selected placeholder (like `0.0`) lets you type over it.
                selection: { anchor: anchor - (item.selectLen ?? 0), head: anchor },
              })
              if (item.retrigger) queueMicrotask(() => startCompletion(view))
            }
          : insert,
        info: item.signature || item.docs ? () => renderDocTooltip(item.signature ?? '', item.docs ?? '') : undefined,
      }
    })

    // The backend filters and ranks by substring and caps the list, so we ask it again on every keystroke
    // instead of letting CodeMirror refilter. CodeMirror's fuzzy matcher would also drop every namespaced ID
    // after one typed letter, since it matches a single character only at the start of a label.
    const typed = text.slice(res.from, context.pos).toLowerCase()
    return {
      from: res.from,
      to: res.to,
      options,
      filter: false,
      getMatch: (completion) => {
        const at = typed ? completion.label.toLowerCase().indexOf(typed) : -1
        return at < 0 ? [] : [at, at + typed.length]
      },
    }
  }
}

function hoverSource(path: string) {
  return async (view: EditorView, pos: number): Promise<Tooltip | null> => {
    if (datapack.status !== 'ready') return null
    const hover = await api.datapackHover(path, view.state.doc.toString(), pos).catch(() => null)
    if (!hover || (!hover.signature && !hover.docs)) return null
    return {
      pos: Math.max(0, Math.min(hover.from, view.state.doc.length)),
      end: Math.max(0, Math.min(hover.to, view.state.doc.length)),
      above: true,
      create: () => ({ dom: renderDocTooltip(hover.signature, hover.docs) }),
    }
  }
}

export function datapackExtensions(path: string): Extension[] {
  if (!isDatapackJson(path)) return []
  const warmPlugin = ViewPlugin.fromClass(
    class {
      constructor() {
        queueMicrotask(() => ensureDatapack())
      }

      update(update: ViewUpdate) {
        if (datapack.status !== 'ready' || completionStatus(update.state) != null) return
        const sel = update.state.selection.main
        if (!sel.empty) return
        const pos = sel.head
        const before = pos > 0 ? update.state.sliceDoc(pos - 1, pos) : ''
        const after = update.state.sliceDoc(pos, pos + 1)
        const deleted = update.docChanged && update.transactions.some((tr) => tr.isUserEvent('delete.backward'))
        const placedInEmptyQuotes = !update.docChanged && update.selectionSet && before === '"' && after === '"'
        if (placedInEmptyQuotes || (deleted && (before === '"' || after === '"' || /[A-Za-z0-9_./:#-]/.test(before)))) {
          queueMicrotask(() => startCompletion(update.view))
        }
      }
    },
  )

  return [
    warmPlugin,
    autocompletion({
      override: [completionSource(path)],
      activateOnTyping: true,
      interactionDelay: 0,
      maxRenderedOptions: 100,
    }),
    hoverTooltip(hoverSource(path), { hoverTime: 350 }),
  ]
}
