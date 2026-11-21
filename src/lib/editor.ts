import { EditorState, Prec, type Extension } from '@codemirror/state'
import { EditorView, ViewPlugin, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, drawSelection, highlightSpecialChars, rectangularSelection, crosshairCursor, dropCursor } from '@codemirror/view'
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands'
import { bracketMatching, foldGutter, foldKeymap, indentOnInput, syntaxHighlighting, HighlightStyle, StreamLanguage } from '@codemirror/language'
import { searchKeymap, highlightSelectionMatches } from '@codemirror/search'
import { acceptCompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from '@codemirror/autocomplete'
import { json } from '@codemirror/lang-json'
import { javascript } from '@codemirror/lang-javascript'
import { markdown } from '@codemirror/lang-markdown'
import { yaml } from '@codemirror/lang-yaml'
import { toml } from '@codemirror/legacy-modes/mode/toml'
import { properties } from '@codemirror/legacy-modes/mode/properties'
import { java } from '@codemirror/legacy-modes/mode/clike'
import { tags } from '@lezer/highlight'
import { forEachDiagnostic, lintKeymap } from '@codemirror/lint'
import { lintFor } from './lint'
import { probejsExtensions } from './probejs.svelte'

const highlight = HighlightStyle.define([
  { tag: [tags.keyword, tags.bool, tags.null, tags.atom], color: 'var(--forest)', fontWeight: '700' },
  { tag: [tags.string, tags.special(tags.string)], color: 'var(--ochre-ink)' },
  { tag: [tags.number, tags.integer, tags.float], color: 'var(--warn)' },
  { tag: [tags.comment, tags.lineComment, tags.blockComment], color: 'var(--muted)', fontStyle: 'italic' },
  { tag: [tags.propertyName, tags.definition(tags.propertyName), tags.attributeName], color: 'var(--text)', fontWeight: '600' },
  { tag: [tags.heading, tags.strong], fontWeight: '700' },
  { tag: tags.emphasis, fontStyle: 'italic' },
  { tag: [tags.link, tags.url], color: 'var(--forest)', textDecoration: 'underline' },
  { tag: [tags.bracket, tags.punctuation, tags.separator], color: 'var(--muted)' },
  { tag: tags.invalid, color: 'var(--fail)' },
])

const theme = EditorView.theme({
  '&': { height: '100%', backgroundColor: 'var(--field)', color: 'var(--text)', fontSize: 'var(--step--1)' },
  '.cm-scroller': { fontFamily: 'var(--font-log)', lineHeight: '1.55' },
  '.cm-content': { caretColor: 'var(--forest)', padding: '8px 0' },
  '.cm-cursor': { borderLeftColor: 'var(--forest)', borderLeftWidth: '2px' },
  '&.cm-focused': { outline: '1px solid var(--forest)', outlineOffset: '-1px' },
  '.cm-gutters': { backgroundColor: 'var(--paper)', color: 'var(--muted)', borderRight: '1px solid var(--line)' },
  '.cm-activeLine': { backgroundColor: 'color-mix(in srgb, var(--here) 45%, transparent)' },
  '.cm-activeLineGutter': { backgroundColor: 'var(--here)', color: 'var(--text)' },
  '&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection': { backgroundColor: 'var(--highlight) !important' },
  '.cm-selectionMatch': { backgroundColor: 'var(--hit-soft)' },
  '.cm-matchingBracket': { backgroundColor: 'var(--ok-fill)', outline: '1px solid var(--ok-border)' },
  '.cm-searchMatch': { backgroundColor: 'var(--hit-soft)', outline: '1px solid var(--hit-edge)' },
  // The selected match is a solid marker in both themes, so we force dark ink over the syntax colors.
  '.cm-searchMatch.cm-searchMatch-selected, .cm-searchMatch.cm-searchMatch-selected *': { backgroundColor: 'var(--hit)', color: 'var(--hit-ink) !important' },
  '.cm-panels': { backgroundColor: 'var(--surface)', color: 'var(--text)', borderColor: 'var(--line)' },
  '.cm-panel input, .cm-panel button': { fontFamily: 'var(--font-ui)', fontSize: 'var(--step--1)' },
  // CodeMirror's base theme paints its own light buttons and fields, which leave light text
  // unreadable in dark mode, so we restyle them like the rest of the app.
  '.cm-textfield': { margin: '0', padding: '3px 8px', color: 'var(--text)', backgroundColor: 'var(--field)', border: '1px solid var(--edge)', borderRadius: 'var(--radius-control)' },
  '.cm-button': {
    margin: '0',
    padding: '3px 10px',
    fontWeight: '400',
    color: 'var(--text)',
    backgroundColor: 'var(--control)',
    backgroundImage: 'none',
    border: '1px solid var(--control-edge)',
    borderRadius: 'var(--radius-control)',
  },
  '.cm-button:hover': { backgroundColor: 'var(--control-hi)' },
  '.cm-button:active': { backgroundImage: 'none', backgroundColor: 'var(--here)' },
  // The search panel separates its find and replace rows with a <br>, which flex layout
  // ignores, so a full-width pseudo-element ordered between the rows breaks the line instead.
  '.cm-panel.cm-search': { display: 'flex', flexWrap: 'wrap', alignItems: 'center', gap: '6px', padding: '8px 40px 8px 10px' },
  '.cm-panel.cm-search::after': { content: '""', order: '1', flexBasis: '100%', height: '0', margin: '-3px 0' },
  '.cm-panel.cm-search br': { display: 'none' },
  '.cm-panel.cm-search [name=replace], .cm-panel.cm-search [name=replaceAll]': { order: '2' },
  '.cm-panel.cm-search input, .cm-panel.cm-search button, .cm-panel.cm-search label': { margin: '0' },
  '.cm-panel.cm-search .cm-textfield': { flex: '0 1 18em', minWidth: '8em' },
  '.cm-panel.cm-search label': { display: 'inline-flex', alignItems: 'center', gap: '4px', fontSize: 'var(--step--1)', color: 'var(--text)', cursor: 'pointer' },
  '.cm-panel.cm-search label:first-of-type': { marginLeft: '8px' },
  '.cm-panel.cm-search input[type=checkbox]': { margin: '0', accentColor: 'var(--forest)' },
  '.cm-panel.cm-search [name=close]': {
    top: '6px',
    right: '8px',
    width: '26px',
    height: '26px',
    fontSize: 'var(--step-1)',
    lineHeight: '1',
    color: 'var(--muted)',
    borderRadius: 'var(--radius-control)',
    cursor: 'pointer',
  },
  '.cm-panel.cm-search [name=close]:hover': { color: 'var(--text)', backgroundColor: 'var(--control)' },
  '.cm-foldGutter span': { color: 'var(--muted)' },
  '.cm-lintRange': { backgroundImage: 'none', textDecoration: 'underline wavy', textDecorationSkipInk: 'none', textUnderlineOffset: '3px' },
  '.cm-lintRange-error': { textDecorationColor: 'var(--fail-icon)' },
  '.cm-lintRange-warning': { textDecorationColor: 'var(--warn-icon)' },
  '.cm-lintRange-active': { backgroundColor: 'var(--hit-soft)' },
  '.cm-gutter-lint': { width: '1.2em' },
  '.cm-gutter-lint .cm-gutterElement': { display: 'flex', alignItems: 'center', justifyContent: 'center', padding: '0' },
  '.cm-lint-marker': { content: 'none', width: '8px', height: '8px', borderRadius: '50%' },
  '.cm-lint-marker-error': { backgroundColor: 'var(--fail-icon)' },
  '.cm-lint-marker-warning': { border: '2px solid var(--warn-icon)' },
  '.cm-tooltip': { backgroundColor: 'var(--surface)', color: 'var(--text)', border: '1px solid var(--line-strong)', borderRadius: 'var(--radius-control)', overflow: 'hidden' },
  '.cm-diagnostic': { fontFamily: 'var(--font-ui)', padding: '6px 10px 6px 8px' },
  '.cm-diagnostic-error': { borderLeft: '4px solid var(--fail-icon)' },
  '.cm-diagnostic-warning': { borderLeft: '4px solid var(--warn-icon)' },
  '.cm-panel.cm-panel-lint': { borderTop: 'none' },
  '.cm-panel.cm-panel-lint ul': { maxHeight: '140px' },
  '.cm-panel.cm-panel-lint ul [aria-selected]': { backgroundColor: 'var(--here)', color: 'var(--text)' },
  '.cm-panel.cm-panel-lint ul:focus [aria-selected]': { backgroundColor: 'var(--highlight)', color: 'var(--text)' },
  '.cm-panel.cm-panel-lint [name=close]': {
    top: '8px',
    right: '6px',
    width: '22px',
    height: '22px',
    color: 'var(--muted)',
    borderRadius: 'var(--radius-control)',
    cursor: 'pointer',
  },
  '.cm-panel.cm-panel-lint [name=close]:hover': { color: 'var(--text)', backgroundColor: 'var(--control)' },
  '.cm-lint-resize': {
    position: 'relative',
    height: '7px',
    cursor: 'row-resize',
    touchAction: 'none',
    backgroundColor: 'var(--surface)',
  },
  '.cm-lint-resize::before': {
    content: '""',
    position: 'absolute',
    left: '0',
    right: '0',
    top: '50%',
    height: '1px',
    transform: 'translateY(-50%)',
    backgroundColor: 'var(--line)',
    transition: 'background-color 0.12s ease, height 0.12s ease',
  },
  '.cm-lint-resize:hover::before': { backgroundColor: 'var(--edge)', height: '2px' },
  '.cm-lint-resize:focus-visible': { outline: 'none' },
  '.cm-lint-resize.dragging::before, .cm-lint-resize:focus-visible::before': { height: '2px', backgroundColor: 'var(--forest)' },
  '@media (prefers-reduced-motion: reduce)': { '.cm-lint-resize::before': { transition: 'none' } },
  '.cm-tooltip-autocomplete': { backgroundColor: 'var(--surface)', color: 'var(--text)', border: '1px solid var(--line-strong)', borderRadius: 'var(--radius-control)', boxShadow: '0 6px 20px var(--shadow)' },
  '.cm-tooltip-autocomplete > ul': { fontFamily: 'var(--font-log)', fontSize: 'var(--step--1)', maxHeight: '16em' },
  '.cm-tooltip-autocomplete > ul > li': { padding: '3px 8px', display: 'flex', alignItems: 'center', gap: '8px' },
  '.cm-tooltip-autocomplete > ul > li[aria-selected]': { backgroundColor: 'var(--here)', color: 'var(--text)' },
  '.cm-completionMatchedText': { color: 'var(--forest)', fontWeight: '700', textDecoration: 'none' },
  '.cm-completionDetail': { color: 'var(--muted)', fontStyle: 'normal', marginLeft: 'auto', fontSize: '0.9em' },
  '.cm-completionInfo': { backgroundColor: 'var(--surface)', color: 'var(--text)', border: '1px solid var(--line-strong)', borderRadius: 'var(--radius-control)', padding: '6px 10px', maxWidth: '420px', maxHeight: '240px', overflow: 'auto' },
  '.cm-probejs-tip': { padding: '6px 10px', maxWidth: '480px', maxHeight: '240px', overflow: 'auto', fontFamily: 'var(--font-ui)', fontSize: 'var(--step--1)', lineHeight: '1.45' },
  '.cm-completionInfo .cm-probejs-tip': { padding: '0' },
  '.cm-probejs-sig': { margin: '0', fontFamily: 'var(--font-log)', fontSize: 'var(--step--1)', whiteSpace: 'pre-wrap', wordBreak: 'break-word', color: 'var(--forest)' },
  '.cm-probejs-doc': { margin: '6px 0 0', color: 'var(--muted)', whiteSpace: 'pre-wrap' },
})

// CodeMirror ignores typing in a read-only editor without a sign, so we report each attempt
// for the view to explain.
function blockedEdits(onBlocked: () => void) {
  const editKeys = new Set(['Backspace', 'Delete', 'Enter', 'Tab'])
  return EditorView.domEventHandlers({
    keydown: (event) => {
      const command = event.ctrlKey || event.metaKey
      const typing = event.key.length === 1 && !command && !event.altKey
      const editing = editKeys.has(event.key) || (command && ['v', 'x'].includes(event.key.toLowerCase()))
      if (typing || editing) onBlocked()
      return false
    },
    drop: () => (onBlocked(), false),
  })
}

// CodeMirror's default panel labels are lowercase shorthand like "all" and "regexp".
const phrases = EditorState.phrases.of({
  next: 'Next',
  previous: 'Previous',
  all: 'Select all',
  'match case': 'Match case',
  regexp: 'Regex',
  'by word': 'Whole word',
  replace: 'Replace',
  'replace all': 'Replace all',
  close: 'Close',
  'Go to line': 'Go to line',
  go: 'Go',
})

function language(path: string): Extension {
  const ext = path.slice(path.lastIndexOf('.') + 1).toLowerCase()
  switch (ext) {
    case 'json':
    case 'jsonc':
    case 'json5':
    case 'mcmeta':
      return json()
    case 'toml':
      return StreamLanguage.define(toml)
    case 'properties':
    case 'cfg':
    case 'ini':
    case 'conf':
      return StreamLanguage.define(properties)
    case 'yml':
    case 'yaml':
      return yaml()
    case 'js':
    case 'mjs':
    case 'ts':
      return javascript({ typescript: ext === 'ts' })
    case 'md':
      return markdown()
    // We show decompiled Java source for class files.
    case 'class':
    case 'java':
      return StreamLanguage.define(java)
    default:
      return []
  }
}

export interface Problems {
  errors: number
  warnings: number
}

function countProblems(state: EditorState): Problems {
  const problems = { errors: 0, warnings: 0 }
  forEachDiagnostic(state, (d) => {
    if (d.severity === 'error') problems.errors++
    else if (d.severity === 'warning') problems.warnings++
  })
  return problems
}

export interface EditorOptions {
  readOnly: boolean
  /** Leaves out the features that scan the whole file, for files over the size you set. */
  simplified: boolean
  onBlocked: () => void
  onLintPaused: () => void
}

const LINT_HEIGHT_KEY = 'packraven.lintPanelHeight'
const LINT_MIN_HEIGHT = 72
const LINT_DEFAULT_HEIGHT = 140
const LINT_STEP = 16
const LINT_BIG_STEP = 64

function readLintHeight(): number | null {
  try {
    const stored = Number(localStorage.getItem(LINT_HEIGHT_KEY) ?? NaN)
    return Number.isFinite(stored) ? stored : null
  } catch {
    return null
  }
}

function saveLintHeight(px: number | null) {
  try {
    if (px === null) localStorage.removeItem(LINT_HEIGHT_KEY)
    else localStorage.setItem(LINT_HEIGHT_KEY, String(Math.round(px)))
  } catch {}
}

function lintPanelResize(): Extension {
  return ViewPlugin.fromClass(
    class {
      private observer: MutationObserver

      constructor(private view: EditorView) {
        this.enhance()
        this.observer = new MutationObserver(() => this.enhance())
        this.observer.observe(view.dom, { childList: true, subtree: true })
      }

      private enhance() {
        const panel = this.view.dom.querySelector<HTMLElement>('.cm-panel.cm-panel-lint')
        if (!panel || panel.querySelector('.cm-lint-resize')) return
        const list = panel.querySelector<HTMLElement>('ul')
        if (!list) return

        const maxHeight = () => Math.max(180, Math.floor(this.view.dom.clientHeight * 0.7))
        const clampHeight = (px: number) => Math.min(Math.max(px, LINT_MIN_HEIGHT), maxHeight())

        const handle = document.createElement('div')
        handle.className = 'cm-lint-resize'
        handle.setAttribute('role', 'separator')
        handle.setAttribute('tabindex', '0')
        handle.setAttribute('aria-orientation', 'horizontal')
        handle.setAttribute('aria-label', 'Resize the problems panel')
        handle.title = 'Drag to resize. Double-click to reset.'

        const syncAria = (px: number) => {
          const max = maxHeight()
          handle.setAttribute('aria-valuemin', String(LINT_MIN_HEIGHT))
          handle.setAttribute('aria-valuemax', String(max))
          handle.setAttribute('aria-valuenow', String(Math.round(px)))
        }

        const apply = (px: number | null) => {
          if (px === null) {
            list.style.height = ''
            list.style.maxHeight = `${LINT_DEFAULT_HEIGHT}px`
            syncAria(list.getBoundingClientRect().height || LINT_DEFAULT_HEIGHT)
          } else {
            const clamped = clampHeight(px)
            list.style.height = `${clamped}px`
            list.style.maxHeight = 'none'
            syncAria(clamped)
          }
          this.view.requestMeasure()
        }

        let drag: { y: number; height: number } | null = null

        handle.addEventListener('pointerdown', (event) => {
          if (event.button !== 0) return
          event.preventDefault()
          handle.setPointerCapture(event.pointerId)
          handle.classList.add('dragging')
          drag = { y: event.clientY, height: list.getBoundingClientRect().height || LINT_DEFAULT_HEIGHT }
        })

        handle.addEventListener('pointermove', (event) => {
          if (!drag) return
          const next = clampHeight(drag.height + (drag.y - event.clientY))
          apply(next)
          saveLintHeight(next)
        })

        const endDrag = () => {
          if (!drag) return
          drag = null
          handle.classList.remove('dragging')
        }
        handle.addEventListener('pointerup', endDrag)
        handle.addEventListener('pointercancel', endDrag)

        handle.addEventListener('dblclick', () => {
          saveLintHeight(null)
          apply(null)
        })

        handle.addEventListener('keydown', (event) => {
          const step = event.shiftKey ? LINT_BIG_STEP : LINT_STEP
          const current = list.getBoundingClientRect().height || LINT_DEFAULT_HEIGHT
          let next: number | null | undefined
          if (event.key === 'ArrowUp') next = clampHeight(current + step)
          else if (event.key === 'ArrowDown') next = clampHeight(current - step)
          else if (event.key === 'Home') next = LINT_MIN_HEIGHT
          else if (event.key === 'End') next = maxHeight()
          else if (event.key === 'Enter') next = null
          else return
          event.preventDefault()
          saveLintHeight(next)
          apply(next)
        })

        panel.prepend(handle)
        apply(readLintHeight())
      }

      destroy() {
        this.observer.disconnect()
      }
    },
  )
}

export function createState(
  path: string,
  doc: string,
  onChange: (state: EditorState) => void,
  onSave: () => void,
  onProblems: (problems: Problems) => void,
  { readOnly, simplified, onBlocked, onLintPaused }: EditorOptions,
): EditorState {
  let reported = ''
  return EditorState.create({
    doc,
    extensions: [
      lineNumbers(),
      highlightActiveLineGutter(),
      highlightSpecialChars(),
      history(),
      simplified ? [] : foldGutter(),
      drawSelection(),
      dropCursor(),
      EditorState.allowMultipleSelections.of(true),
      indentOnInput(),
      bracketMatching(),
      closeBrackets(),
      rectangularSelection(),
      crosshairCursor(),
      highlightActiveLine(),
      highlightSelectionMatches(),
      syntaxHighlighting(highlight),
      language(path),
      simplified ? [] : lintFor(path, onLintPaused),
      simplified ? [] : lintPanelResize(),
      simplified || readOnly ? [] : probejsExtensions(path),
      EditorState.readOnly.of(readOnly),
      readOnly ? blockedEdits(onBlocked) : [],
      theme,
      phrases,
      EditorView.contentAttributes.of({ 'aria-label': path.split(/[\\/]/).pop()! }),
      Prec.highest(keymap.of([{ key: 'Tab', run: acceptCompletion }])),
      keymap.of([
        { key: 'Mod-s', preventDefault: true, run: () => (onSave(), true) },
        ...completionKeymap,
        indentWithTab,
        ...closeBracketsKeymap,
        ...defaultKeymap,
        ...searchKeymap,
        ...historyKeymap,
        ...(simplified ? [] : foldKeymap),
        ...lintKeymap,
      ]),
      EditorView.updateListener.of((update) => {
        if (update.docChanged) onChange(update.state)
        const problems = countProblems(update.state)
        const key = `${problems.errors}/${problems.warnings}`
        if (key !== reported) {
          reported = key
          onProblems(problems)
        }
      }),
    ],
  })
}
