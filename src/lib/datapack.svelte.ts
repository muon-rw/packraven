import type { EditorState, Extension, Text } from '@codemirror/state'
import { syntaxTree } from '@codemirror/language'
import { ViewPlugin, hoverTooltip, type EditorView, type Tooltip, type ViewUpdate } from '@codemirror/view'
import { autocompletion, completionStatus, startCompletion, type Completion, type CompletionContext, type CompletionResult } from '@codemirror/autocomplete'
import { openUrl } from '@tauri-apps/plugin-opener'
import { api, errorText, type DatapackStatusInfo } from './api'
import { renderMarkdown } from './markdown'

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
  if (lower === 'fabric.mod.json' || lower.endsWith('/fabric.mod.json')) return true
  if (lower === 'fabric_loader_dependencies.json' || lower.endsWith('/fabric_loader_dependencies.json')) return true
  if (/(?:^|\/)assets\/[^/]+\/textures\/.+\.png\.mcmeta$/i.test(norm)) return true
  // Minecraft only loads `.json` data and asset files, so `.jsonc` files get the plain JSON checks.
  if (!lower.endsWith('.json')) return false
  return /(?:^|\/)(?:data\/[^/]+\/[^/]+\/.+|assets\/[^/]+\/.+)\.json$/i.test(norm)
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

export function applyDatapackStatus(info: DatapackStatusInfo): boolean {
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
      return applyDatapackStatus(await api.datapackStatus(force))
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
    const doc = document.createElement('div')
    doc.className = 'cm-probejs-doc cm-mcdoc-md'
    doc.innerHTML = renderMarkdown(docs)
    doc.addEventListener('click', (event) => {
      const link = (event.target as Element | null)?.closest<HTMLAnchorElement>('a[href]')
      if (!link) return
      event.preventDefault()
      void openUrl(link.href)
    })
    box.appendChild(doc)
  }
  return box
}

interface StringContext {
  from: number
  to: number
  isValue: boolean
}

function stringContextAt(state: EditorState): StringContext | null {
  const sel = state.selection.main
  const tree = syntaxTree(state)
  const check = (pos: number, side: -1 | 1): StringContext | null => {
    for (let node: ReturnType<typeof tree.resolveInner> | null = tree.resolveInner(pos, side); node; node = node.parent) {
      if (node.name === 'String') {
        if (sel.from > node.from && sel.to < node.to) {
          return { from: node.from, to: node.to, isValue: true }
        }
        return null
      }
      if (node.name === 'PropertyName') {
        if (sel.empty && sel.from === node.from + 1 && node.to === node.from + 2) {
          return { from: node.from, to: node.to, isValue: false }
        }
        return null
      }
    }
    return null
  }

  const found = check(sel.from, 1) ?? check(sel.from, -1)
  if (found) return found

  // Fallback when Lezer hasn't parsed an empty `""` pair yet.
  if (sel.empty && sel.head > 0 && state.sliceDoc(sel.head - 1, sel.head) === '"' && state.sliceDoc(sel.head, sel.head + 1) === '"') {
    return { from: sel.head - 1, to: sel.head + 1, isValue: true }
  }
  return null
}

// Tracks completions opened automatically by cursor placement (not Ctrl+Space or editing) so we
// can suppress a redundant 1-item popup when the cursor sits at the end of an already-valid ID.
const autoTriggeredViews = new WeakSet<EditorView>()

// Marks CodeMirror Completion objects that came from deprecated mcdoc fields or enum values.
const deprecatedCompletions = new WeakSet<Completion>()

// Strings the backend said have no choices (plain text like a name or description), by start offset.
// Caret moves inside them don't ask again until the document changes.
const stringsWithoutChoices = new WeakMap<EditorView, { doc: Text; starts: Set<number> }>()

function rememberStringWithoutChoices(view: EditorView, doc: Text, from: number) {
  let known = stringsWithoutChoices.get(view)
  if (!known || known.doc !== doc) {
    known = { doc, starts: new Set() }
    stringsWithoutChoices.set(view, known)
  }
  known.starts.add(from)
}

function stringLacksChoices(view: EditorView, state: EditorState, str: StringContext): boolean {
  const known = stringsWithoutChoices.get(view)
  return !!known && known.doc === state.doc && known.starts.has(str.from)
}

function autoTriggerCompletion(view: EditorView) {
  const str = stringContextAt(view.state)
  if (!str || stringLacksChoices(view, view.state, str)) return
  autoTriggeredViews.add(view)
  startCompletion(view)
}

function completionSource(path: string) {
  return async (context: CompletionContext): Promise<CompletionResult | null> => {
    const isAuto = context.view ? autoTriggeredViews.delete(context.view) : false
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
    const sel = context.state.selection.main
    const str = stringContextAt(context.state)
    const queryPos = !sel.empty && str?.isValue ? sel.from : context.pos
    const res = await api.datapackComplete(path, text, queryPos).catch(() => null)
    if (isAuto && context.view && str && res && !res.slotHasChoices) {
      rememberStringWithoutChoices(context.view, context.state.doc, str.from)
    }
    if (context.aborted || !res || !res.items.length) return null

    if (isAuto && sel.empty && res.inString && res.items.length === 1) {
      const fullContent = text.slice(res.from, res.replaceTo ?? res.to)
      if (res.items[0].label === fullContent) return null
    }

    // Accepting replaces the rest of the word or string after the cursor too, which stays the same length while you type.
    const resultTo = Math.max(res.to, !sel.empty && str?.isValue ? sel.to : res.to)
    const replaceEnd = Math.max(res.replaceTo ?? res.to, resultTo)
    const suffix = Math.max(0, replaceEnd - resultTo)
    const options: Completion[] = res.items.map((item) => {
      const insert = item.insertText ?? item.label
      const customApply = suffix > 0 || item.cursorOffset != null || !!item.selectLen || item.consumeClosingQuote || item.retrigger
      const completion: Completion = {
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
      if (item.deprecated) deprecatedCompletions.add(completion)
      return completion
    })

    // The backend filters and ranks by substring and caps the list, so we ask it again on every keystroke
    // instead of letting CodeMirror refilter. CodeMirror's fuzzy matcher would also drop every namespaced ID
    // after one typed letter, since it matches a single character only at the start of a label.
    const typed = text.slice(res.from, queryPos).toLowerCase()
    return {
      from: res.from,
      to: resultTo,
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
    if (datapack.status !== 'ready' && !(await ensureDatapack())) return null
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

export function datapackExtensions(path: string, readOnly = false): Extension[] {
  if (!isDatapackJson(path)) return []
  const hover = hoverTooltip(hoverSource(path), { hoverTime: 350 })
  if (readOnly) {
    queueMicrotask(() => void ensureDatapack())
    return [hover]
  }
  const warmPlugin = ViewPlugin.fromClass(
    class {
      private destroyed = false
      private dismissedString: { from: number; to: number } | null = null

      constructor(private view: EditorView) {
        queueMicrotask(() => {
          ensureDatapack().then((ok) => {
            if (!ok || this.destroyed || !this.view.hasFocus || completionStatus(this.view.state) != null) return
            autoTriggerCompletion(this.view)
          })
        })
      }

      update(update: ViewUpdate) {
        // An auto-trigger whose source never ran must not mark your next request as automatic too.
        if (update.docChanged || update.selectionSet) autoTriggeredViews.delete(update.view)
        if (update.docChanged) {
          this.dismissedString = null
        } else if (!update.selectionSet && update.view.hasFocus && completionStatus(update.startState) === 'active' && completionStatus(update.state) == null) {
          const str = stringContextAt(update.state)
          if (str) this.dismissedString = { from: str.from, to: str.to }
        }

        const str = stringContextAt(update.state)
        if (this.dismissedString && (!str || str.from !== this.dismissedString.from || str.to !== this.dismissedString.to)) {
          this.dismissedString = null
        }

        if (datapack.status !== 'ready' || completionStatus(update.state) != null) return

        const deleted = update.docChanged && update.transactions.some((tr) => tr.isUserEvent('delete'))
        if (deleted) {
          const sel = update.state.selection.main
          if (!sel.empty) return
          const pos = sel.head
          const before = pos > 0 ? update.state.sliceDoc(pos - 1, pos) : ''
          const after = update.state.sliceDoc(pos, pos + 1)
          if (str || before === '"' || after === '"' || /[A-Za-z0-9_./:#-]/.test(before)) {
            queueMicrotask(() => !this.destroyed && startCompletion(update.view))
          }
          return
        }

        if (!update.docChanged && update.selectionSet && str) {
          if (this.dismissedString && this.dismissedString.from === str.from && this.dismissedString.to === str.to) return
          queueMicrotask(() => !this.destroyed && autoTriggerCompletion(update.view))
        }
      }

      destroy() {
        this.destroyed = true
      }
    },
  )

  return [
    warmPlugin,
    autocompletion({
      override: [completionSource(path)],
      activateOnTyping: true,
      activateOnTypingDelay: 25,
      interactionDelay: 0,
      maxRenderedOptions: 100,
      optionClass: (c) => (deprecatedCompletions.has(c) ? 'cm-completion-deprecated' : ''),
    }),
    hover,
  ]
}
