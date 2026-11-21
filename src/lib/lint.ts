import { Facet, StateEffect, type Extension, type Text } from '@codemirror/state'
import type { EditorView, ViewUpdate } from '@codemirror/view'
import { forceLinting, linter, lintGutter, type Diagnostic } from '@codemirror/lint'
import { createScanner, parseTree, printParseErrorCode, type Node, type ParseError } from 'jsonc-parser'
import JSON5 from 'json5'
import { parse as parseToml, TomlError } from 'smol-toml'
import { parseAllDocuments } from 'yaml'
import { checkKubejsAst, isKubejsScript } from './kubejs'
import { currentKubejsContext, loadKubejsContext } from './kubejsContext'

type Check = (doc: Text) => Diagnostic[]

// Prettier's parser is large, so we load it only once you open a script.
const javascript = () => import('./javascript')

// Each check re-parses the whole file, so past this long a pass stalls typing noticeably. How
// long a pass takes depends on the device as much as the file, so we measure it.
const SLOW_CHECK_MS = 150

const checkOnce = Facet.define<() => void>()
const recheck = StateEffect.define<null>()

/**
 * Returns a linter for the file type that turns itself off, and calls `onPaused`, once a single
 * check runs slow. After that it only checks when you run {@link checkNow}.
 */
export function lintFor(path: string, onPaused: () => void): Extension {
  const check = checkFor(path)
  if (!check) return []
  let paused = false
  let forced = false
  const source = async (view: EditorView) => {
    if (paused && !forced) return []
    forced = false
    const doc = view.state.doc
    const run = await check
    const started = performance.now()
    const diagnostics = run(doc)
    if (!paused && performance.now() - started > SLOW_CHECK_MS) {
      paused = true
      onPaused()
    }
    return diagnostics
  }
  const needsRefresh = (update: ViewUpdate) => update.transactions.some((tr) => tr.effects.some((e) => e.is(recheck)))
  return [linter(source, { delay: 300, needsRefresh }), lintGutter(), checkOnce.of(() => (forced = true))]
}

/** Checks the file once, even when checking as you type has paused. */
// forceLinting only hurries a check that's already scheduled, so we schedule one first.
export function checkNow(view: EditorView) {
  for (const allow of view.state.facet(checkOnce)) allow()
  view.dispatch({ effects: recheck.of(null) })
  forceLinting(view)
}

function checkFor(path: string): Check | Promise<Check> | null {
  const name = path.slice(path.lastIndexOf('/') + 1).toLowerCase()
  const ext = name.slice(name.lastIndexOf('.') + 1)
  switch (ext) {
    case 'json':
      return (doc) => checkJson(doc, false)
    case 'jsonc':
      return (doc) => checkJson(doc, true)
    case 'mcmeta':
      return name === 'pack.mcmeta' ? checkPackMcmeta : (doc) => checkJson(doc, false)
    case 'json5':
      return checkJson5
    case 'toml':
      return checkToml
    case 'yml':
    case 'yaml':
      return checkYaml
    case 'properties':
      return checkProperties
    case 'js':
    case 'mjs':
      // KubeJS checks use the pack's ProbeJS typings, so the first check waits for them.
      return Promise.all([javascript(), isKubejsScript(path) ? loadKubejsContext() : null]).then(([js]) => (doc) => checkJs(doc, js, path))
    default:
      return null
  }
}

function offsetAt(doc: Text, line: number, column: number): number {
  const at = doc.line(Math.min(Math.max(line, 1), doc.lines))
  return Math.min(at.from + Math.max(column - 1, 0), at.to)
}

function clamp(doc: Text, from: number, to: number): { from: number; to: number } {
  const start = Math.min(Math.max(from, 0), doc.length)
  return { from: start, to: Math.min(Math.max(to, start), doc.length) }
}

const jsonErrors: Record<string, string> = {
  InvalidSymbol: 'Unexpected character',
  InvalidNumberFormat: 'Invalid number',
  PropertyNameExpected: 'Expected a quoted key',
  ValueExpected: 'Expected a value',
  ColonExpected: 'Expected a colon',
  CommaExpected: 'Expected a comma',
  CloseBraceExpected: 'Expected a closing brace',
  CloseBracketExpected: 'Expected a closing bracket',
  EndOfFileExpected: 'Unexpected content after the end of the document',
  InvalidCommentToken: 'Invalid comment',
  UnexpectedEndOfComment: 'Unclosed comment',
  UnexpectedEndOfString: 'Unclosed string',
  UnexpectedEndOfNumber: 'Incomplete number',
  InvalidUnicode: 'Invalid unicode escape',
  InvalidEscapeCharacter: 'Invalid escape sequence',
  InvalidCharacter: 'Invalid character in string',
}

// jsonc-parser declares SyntaxKind as a const enum, which verbatimModuleSyntax won't let us import.
const Token = { CloseBrace: 2, CloseBracket: 4, Comma: 5, LineComment: 12, BlockComment: 13, LineBreak: 14, Trivia: 15, EOF: 17 } as const

function checkJson(doc: Text, allowExtensions: boolean): Diagnostic[] {
  const { diagnostics } = parseJson(doc, allowExtensions)
  return diagnostics
}

// We parse leniently (comments and trailing commas allowed) so one stray comma doesn't
// hide every later error, then flag those extensions separately as warnings.
function parseJson(doc: Text, allowExtensions: boolean): { root: Node | undefined; diagnostics: Diagnostic[] } {
  const text = doc.toString()
  const errors: ParseError[] = []
  const root = parseTree(text, errors, { allowTrailingComma: true, disallowComments: false })
  const diagnostics: Diagnostic[] = errors.map((e) => ({
    ...clamp(doc, e.offset, e.offset + e.length),
    severity: 'error',
    message: jsonErrors[printParseErrorCode(e.error)] ?? 'Invalid JSON',
  }))
  if (!allowExtensions) diagnostics.push(...nonStandardJson(doc, text))
  if (root) diagnostics.push(...duplicateJsonKeys(doc, root))
  return { root, diagnostics }
}

function nonStandardJson(doc: Text, text: string): Diagnostic[] {
  const diagnostics: Diagnostic[] = []
  const scanner = createScanner(text, false)
  let comma: number | null = null
  for (let kind = scanner.scan(); kind !== Token.EOF; kind = scanner.scan()) {
    const from = scanner.getTokenOffset()
    const to = from + scanner.getTokenLength()
    if (kind === Token.LineComment || kind === Token.BlockComment) {
      diagnostics.push({ from, to, severity: 'warning', message: "Standard JSON doesn't allow comments, so strict parsers will reject this file." })
      continue
    }
    if (kind === Token.Trivia || kind === Token.LineBreak) continue
    if (comma !== null && (kind === Token.CloseBrace || kind === Token.CloseBracket)) {
      diagnostics.push({ from: comma, to: comma + 1, severity: 'warning', message: "Standard JSON doesn't allow trailing commas, so strict parsers will reject this file." })
    }
    comma = kind === Token.Comma ? from : null
  }
  return clampAll(doc, diagnostics)
}

function duplicateJsonKeys(doc: Text, root: Node): Diagnostic[] {
  const diagnostics: Diagnostic[] = []
  const visit = (node: Node) => {
    if (node.type === 'object') {
      const seen = new Set<string>()
      for (const property of node.children ?? []) {
        const key = property.children?.[0]
        if (!key || typeof key.value !== 'string') continue
        if (seen.has(key.value)) {
          diagnostics.push({
            from: key.offset,
            to: key.offset + key.length,
            severity: 'warning',
            message: `Duplicate key "${key.value}". Depending on the parser, this either fails or silently keeps only one of the values.`,
          })
        }
        seen.add(key.value)
      }
    }
    node.children?.forEach(visit)
  }
  visit(root)
  return clampAll(doc, diagnostics)
}

function clampAll(doc: Text, diagnostics: Diagnostic[]): Diagnostic[] {
  return diagnostics.map((d) => ({ ...d, ...clamp(doc, d.from, d.to) }))
}

function child(node: Node | undefined, key: string): Node | undefined {
  return node?.children?.find((p) => p.children?.[0]?.value === key)?.children?.[1]
}

function checkPackMcmeta(doc: Text): Diagnostic[] {
  const { root, diagnostics } = parseJson(doc, false)
  if (!root || diagnostics.some((d) => d.severity === 'error')) return diagnostics
  const problem = (node: Node, message: string) => diagnostics.push({ ...clamp(doc, node.offset, node.offset + node.length), severity: 'error', message })
  if (root.type !== 'object') {
    problem(root, 'pack.mcmeta must be a JSON object.')
    return diagnostics
  }
  const pack = child(root, 'pack')
  if (!pack) {
    problem(root, 'Missing the "pack" section, so Minecraft won\'t load this pack.')
    return diagnostics
  }
  if (pack.type !== 'object') {
    problem(pack, '"pack" must be an object.')
    return diagnostics
  }
  // Minecraft 1.21.9 replaced pack_format with min_format/max_format, so either is acceptable.
  const format = child(pack, 'pack_format')
  if (!format && !child(pack, 'min_format')) problem(pack, 'Missing "pack_format" (or "min_format" on Minecraft 1.21.9 and later).')
  else if (format && !(format.type === 'number' && Number.isInteger(format.value))) problem(format, '"pack_format" must be a whole number.')
  const description = child(pack, 'description')
  if (description && !['string', 'object', 'array'].includes(description.type)) problem(description, '"description" must be text, a text component, or a list of them.')
  return diagnostics
}

function checkJson5(doc: Text): Diagnostic[] {
  try {
    JSON5.parse(doc.toString())
    return []
  } catch (e) {
    const error = e as SyntaxError & { lineNumber?: number; columnNumber?: number }
    const message = error.message.replace(/^JSON5: /, '').replace(/ at \d+:\d+$/, '')
    return [errorAt(doc, error.lineNumber ?? 1, error.columnNumber ?? 1, message)]
  }
}

function checkToml(doc: Text): Diagnostic[] {
  try {
    parseToml(doc.toString())
    return []
  } catch (e) {
    if (!(e instanceof TomlError)) throw e
    const message = e.message.replace(/^Invalid TOML document: /, '').split('\n')[0]
    return [errorAt(doc, e.line, e.column, message)]
  }
}

// These parsers stop at the first error and only report where it starts, so we underline
// the rest of the word there to make it visible.
function errorAt(doc: Text, line: number, column: number, message: string): Diagnostic {
  const from = offsetAt(doc, line, column)
  const rest = doc.sliceString(from, doc.lineAt(from).to)
  const word = rest.match(/^[^\s=:,[\]{}"']*/)![0].length
  return { ...clamp(doc, from, from + Math.max(word, 1)), severity: 'error', message: capitalize(message) }
}

function checkJs(doc: Text, js: Awaited<ReturnType<typeof javascript>>, path: string): Diagnostic[] {
  const text = doc.toString()
  let ast: Record<string, unknown>
  try {
    ast = js.parseJs(text)
  } catch (e) {
    const error = js.syntaxErrorOf(e)
    if (!error) throw e
    return [errorAt(doc, error.line, error.column, error.message)]
  }
  if (!isKubejsScript(path)) return []
  return checkKubejsAst(doc, ast, path, currentKubejsContext())
}

function checkYaml(doc: Text): Diagnostic[] {
  const diagnostics: Diagnostic[] = []
  for (const document of parseAllDocuments(doc.toString(), { prettyErrors: false })) {
    for (const [problems, severity] of [[document.errors, 'error'], [document.warnings, 'warning']] as const) {
      for (const problem of problems) {
        const range = clamp(doc, problem.pos[0], Math.max(problem.pos[1], problem.pos[0] + 1))
        const message = problem.code === 'DUPLICATE_KEY' ? `Duplicate key "${doc.sliceString(range.from, range.to)}"` : problem.message.split('\n')[0]
        diagnostics.push({ ...range, severity, message })
      }
    }
  }
  return diagnostics
}

// Mirrors java.util.Properties.load: a line ending in an odd number of backslashes continues
// onto the next with its leading whitespace dropped, the key ends at the first unescaped '=',
// ':' or whitespace, and a malformed \uXXXX escape makes Java reject the whole file.
function checkProperties(doc: Text): Diagnostic[] {
  const diagnostics: Diagnostic[] = []
  const keys = new Map<string, number>()
  for (let number = 1; number <= doc.lines; number++) {
    const first = doc.line(number)
    const start = first.text.search(/\S/)
    if (start < 0 || first.text[start] === '#' || first.text[start] === '!') continue

    let text = ''
    const offsets: number[] = []
    let line = first
    let skip = start
    for (;;) {
      const continues = endsWithContinuation(line.text)
      const end = continues ? line.text.length - 1 : line.text.length
      for (let i = skip; i < end; i++) {
        text += line.text[i]
        offsets.push(line.from + i)
      }
      if (!continues || number >= doc.lines) break
      line = doc.line(++number)
      const indent = line.text.search(/\S/)
      skip = indent < 0 ? line.text.length : indent
    }
    offsets.push(line.to)
    const range = (from: number, to: number) => ({ from: offsets[Math.min(from, text.length)], to: offsets[Math.min(to, text.length)] })

    for (const match of text.matchAll(/\\+u/g)) {
      const backslashes = match[0].length - 1
      if (backslashes % 2 === 0) continue
      const at = match.index + match[0].length
      if (!/^[0-9a-fA-F]{4}/.test(text.slice(at, at + 4))) {
        diagnostics.push({ ...range(at - 2, at + 4), severity: 'error', message: 'Malformed \\uXXXX escape. Java refuses to load the whole file because of this.' })
      }
    }

    let end = 0
    while (end < text.length && !/[=:\s]/.test(text[end])) end += text[end] === '\\' ? 2 : 1
    end = Math.min(end, text.length)
    const key = text.slice(0, end).replace(/\\(.)/g, '$1')
    const keyRange = range(0, end)

    const rest = text.slice(end)
    if (!/^\s*[=:]/.test(rest) && rest.trim()) {
      diagnostics.push({
        ...keyRange,
        severity: 'warning',
        message: `No "=" after "${key}", so Java reads "${key}" as the key and "${rest.trim()}" as its value.`,
      })
    }

    const previous = keys.get(key)
    if (previous !== undefined) {
      diagnostics.push({ ...keyRange, severity: 'warning', message: `Duplicate key "${key}" (first set on line ${previous}). Java keeps only the last value.` })
    } else {
      keys.set(key, first.number)
    }
  }
  return diagnostics
}

function endsWithContinuation(line: string): boolean {
  return line.match(/\\*$/)![0].length % 2 === 1
}

function capitalize(message: string): string {
  return message.charAt(0).toUpperCase() + message.slice(1)
}
