import type { Extension } from '@codemirror/state'
import { ViewPlugin, hoverTooltip, type EditorView, type Tooltip } from '@codemirror/view'
import { autocompletion, type Completion, type CompletionContext, type CompletionResult } from '@codemirror/autocomplete'
import { api, errorText } from './api'
import { isKubejsScript } from './kubejs'
import { resetKubejsContext } from './kubejsContext'

export type ProbejsStatus = 'idle' | 'loading' | 'ready' | 'unavailable' | 'error'

export const probejs = $state({
  status: 'idle' as ProbejsStatus,
  fileCount: 0,
  totalBytes: 0,
  typeCount: 0,
  stringEntries: 0,
  loadMs: 0,
  failedFiles: [] as string[],
  error: null as string | null,
})

// You can run /probejs dump while a script is open, so we look for typings again after a while
// instead of trusting a missing dump for the rest of the session. Completion asks on every
// keystroke, so we wait that long after a failed load too.
const RECHECK_AFTER_MS = 15_000

let loadPromise: Promise<boolean> | null = null
let lastFailedAt = 0

export function resetProbejs() {
  loadPromise = null
  probejs.status = 'idle'
  probejs.fileCount = 0
  probejs.totalBytes = 0
  probejs.typeCount = 0
  probejs.stringEntries = 0
  probejs.loadMs = 0
  probejs.failedFiles = []
  probejs.error = null
}

function failedRecently() {
  return (probejs.status === 'unavailable' || probejs.status === 'error') && Date.now() - lastFailedAt < RECHECK_AFTER_MS
}

export async function ensureProbejs(force = false): Promise<boolean> {
  if (!force && probejs.status === 'ready') return true
  if (!force && failedRecently()) return false
  if (!force && loadPromise) return loadPromise

  probejs.status = 'loading'
  probejs.error = null

  let run!: Promise<boolean>
  run = (async () => {
    try {
      const info = await api.probejsStatus(force)
      probejs.fileCount = info.fileCount
      probejs.totalBytes = info.totalBytes
      probejs.typeCount = info.typeCount
      probejs.stringEntries = info.stringEntries
      probejs.loadMs = info.loadMs
      probejs.failedFiles = info.failedFiles
      if (!info.available) {
        probejs.status = 'unavailable'
        lastFailedAt = Date.now()
        return false
      }
      probejs.status = 'ready'
      return true
    } catch (e) {
      probejs.status = 'error'
      probejs.error = errorText(e)
      lastFailedAt = Date.now()
      return false
    } finally {
      if (loadPromise === run) loadPromise = null
    }
  })()

  loadPromise = run
  return run
}

export async function refreshProbejs(): Promise<boolean> {
  resetKubejsContext()
  return ensureProbejs(true)
}

function triggerCharAt(context: CompletionContext): string | undefined {
  if (context.pos <= 0) return undefined
  const ch = context.state.sliceDoc(context.pos - 1, context.pos)
  return ch === '.' || ch === '"' || ch === "'" || ch === '/' || ch === '#' || ch === '@' || ch === '%' ? ch : undefined
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

function completionSource() {
  return async (context: CompletionContext): Promise<CompletionResult | null> => {
    const trigger = triggerCharAt(context)
    if (!context.explicit && !trigger) {
      const word = context.matchBefore(/[A-Za-z0-9_$:]+/)
      if (!word || word.from === word.to) return null
    }
    if (probejs.status !== 'ready') {
      const ok = await ensureProbejs()
      if (!ok) return null
    }
    const text = context.state.doc.toString()
    const res = await api.probejsComplete(text, context.pos).catch(() => null)
    if (context.aborted || !res || !res.items.length) return null

    const options: Completion[] = res.items.map((item, index) => ({
      label: item.label,
      type: item.kind,
      detail: item.detail ?? undefined,
      apply: item.insertText ?? item.label,
      boost: Math.max(-99, 50 - Math.min(index, 149)),
      info: item.signature || item.docs ? () => renderDocTooltip(item.signature ?? '', item.docs ?? '') : undefined,
    }))

    return {
      from: res.from,
      to: res.to,
      options,
      // String completions search huge registries on the Rust side, so keep filtering fresh as you type.
      validFor: res.inString ? undefined : /^[A-Za-z0-9_$]*$/,
    }
  }
}

function hoverSource() {
  return async (view: EditorView, pos: number): Promise<Tooltip | null> => {
    if (probejs.status !== 'ready') return null
    const hover = await api.probejsHover(view.state.doc.toString(), pos).catch(() => null)
    if (!hover || (!hover.signature && !hover.docs)) return null
    return {
      pos: Math.max(0, Math.min(hover.from, view.state.doc.length)),
      end: Math.max(0, Math.min(hover.to, view.state.doc.length)),
      above: true,
      create: () => ({ dom: renderDocTooltip(hover.signature, hover.docs) }),
    }
  }
}

export function probejsExtensions(path: string): Extension[] {
  if (!isKubejsScript(path)) return []
  const warmPlugin = ViewPlugin.fromClass(
    class {
      // CodeMirror builds plugins inside whatever Svelte effect creates the view, so we start the
      // load afterwards rather than let that effect read the ProbeJS status.
      constructor() {
        queueMicrotask(() => ensureProbejs())
      }
    },
  )

  return [
    warmPlugin,
    autocompletion({
      override: [completionSource()],
      activateOnTyping: true,
      interactionDelay: 0,
      maxRenderedOptions: 100,
    }),
    hoverTooltip(hoverSource(), { hoverTime: 350 }),
  ]
}
