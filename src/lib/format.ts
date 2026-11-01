import type { FileState, PackFile } from './api'
import { isolateHistory } from '@codemirror/commands'
import type { ChangeSpec } from '@codemirror/state'
import type { EditorView } from '@codemirror/view'
import { createScanner, parse as parseJsonc, type ParseError } from 'jsonc-parser'
import { parse as parseToml, TomlError } from 'smol-toml'
import { Lexer, parseAllDocuments } from 'yaml'
import { extensionOf } from './viewers'

export type FormatOutcome = { kind: 'formatted' | 'unchanged' | 'unsupported' | 'unsafe' | 'edited' } | { kind: 'invalid'; line: number | null }

class InvalidAt extends Error {
  line: number | null
  constructor(line: number | null) {
    super('Syntax error')
    this.line = line
  }
}

interface Formatter {
  format: (text: string) => string | Promise<string>
  /** Returns what the text means, ignoring layout. Throws when the text doesn't parse. */
  meaning: (text: string) => unknown
}

async function formatterFor(path: string): Promise<Formatter | null> {
  switch (extensionOf(path)) {
    case 'js':
    case 'mjs': {
      // Prettier is most of the formatting code by size, so we load it only for scripts.
      const js = await import('./javascript')
      const meaning = (text: string) => {
        try {
          return js.jsMeaning(text)
        } catch (e) {
          throw new InvalidAt(js.syntaxErrorOf(e)?.line ?? null)
        }
      }
      return { format: (text) => js.formatJs(text, detectIndent(text, '  ')), meaning }
    }
    case 'json':
    case 'jsonc':
    case 'mcmeta':
      return { format: formatJson, meaning: jsonMeaning }
    case 'toml':
      return { format: formatToml, meaning: tomlMeaning }
    case 'yml':
    case 'yaml':
      return { format: formatYaml, meaning: yamlMeaning }
    default:
      return null
  }
}

export async function formatView(view: EditorView, path: string): Promise<FormatOutcome> {
  const formatter = await formatterFor(path)
  if (!formatter) return { kind: 'unsupported' }
  const doc = view.state.doc
  const before = doc.toString()
  let meaning: string
  try {
    meaning = canonical(formatter.meaning(before))
  } catch (e) {
    return { kind: 'invalid', line: e instanceof InvalidAt ? e.line : null }
  }
  let after: string
  // We compare what the file means before and after, so a formatter bug can't quietly change
  // a config value.
  try {
    after = await formatter.format(before)
    if (canonical(formatter.meaning(after)) !== meaning) return { kind: 'unsafe' }
  } catch {
    return { kind: 'unsafe' }
  }
  if (view.state.doc !== doc) return { kind: 'edited' }
  if (after === before) return { kind: 'unchanged' }
  // CodeMirror merges edits made in quick succession into one undo step, so without isolating
  // it, undo could also take back what you typed just before formatting.
  view.dispatch({ changes: changesBetween(before, after), userEvent: 'format', annotations: isolateHistory.of('full') })
  return { kind: 'formatted' }
}

function canonical(value: unknown): string {
  return JSON.stringify(value, (_, v) => (typeof v === 'bigint' ? `${v}n` : v))
}

// The first indented line in a tidy file is one level deep, so its indent is one unit.
function detectIndent(text: string, fallback: string): string {
  const first = text.match(/^([ \t]+)\S/m)?.[1]
  if (!first) return fallback
  return first[0] === '\t' ? '\t' : first.length <= 8 ? first : fallback
}

// jsonc-parser declares SyntaxKind as a const enum, which verbatimModuleSyntax won't let us import.
const Token = { OpenBrace: 1, CloseBrace: 2, OpenBracket: 3, CloseBracket: 4, Comma: 5, Colon: 6, LineComment: 12, BlockComment: 13, LineBreak: 14, Trivia: 15, EOF: 17 } as const

// A container that already fits on one short line stays there, like `[0, 0, 0]` in a model file.
const INLINE_MAX = 80

interface JsonToken {
  kind: number
  text: string
  from: number
  to: number
  /** Line breaks between this token and the one before it. */
  breaks: number
  close?: number
}

function jsonMeaning(text: string): unknown {
  const errors: ParseError[] = []
  const value = parseJsonc(text, errors, { allowTrailingComma: true, disallowComments: false })
  if (errors.length) throw new InvalidAt(text.slice(0, errors[0].offset).split('\n').length)
  return value
}

function scanJson(text: string): JsonToken[] {
  const tokens: JsonToken[] = []
  const open: number[] = []
  const scanner = createScanner(text, false)
  let breaks = 0
  for (let kind = scanner.scan(); kind !== Token.EOF; kind = scanner.scan()) {
    if (kind === Token.Trivia) continue
    if (kind === Token.LineBreak) {
      breaks++
      continue
    }
    const from = scanner.getTokenOffset()
    const to = from + scanner.getTokenLength()
    if (kind === Token.OpenBrace || kind === Token.OpenBracket) open.push(tokens.length)
    if (kind === Token.CloseBrace || kind === Token.CloseBracket) {
      const opener = open.pop()
      if (opener !== undefined) tokens[opener].close = tokens.length
    }
    tokens.push({ kind, text: text.slice(from, to), from, to, breaks })
    breaks = 0
  }
  return tokens
}

function staysInline(tokens: JsonToken[], open: number): boolean {
  const close = tokens[open].close!
  if (tokens[close].to - tokens[open].from > INLINE_MAX) return false
  for (let i = open + 1; i <= close; i++) {
    if (tokens[i].breaks || tokens[i].text.includes('\n')) return false
  }
  return true
}

// We print from tokens rather than the parsed value so comments and blank lines between
// entries survive.
function formatJson(text: string): string {
  const indent = detectIndent(text, '  ')
  const tokens = scanJson(text)
  const frames: { inline: boolean; object: boolean; empty: boolean }[] = []
  let out = ''
  let pending: 'none' | 'space' | 'line' = 'none'
  let afterOpen = false
  const lineBreak = (depth: number, blank: boolean) => (blank ? '\n\n' : '\n') + indent.repeat(depth)

  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i]
    if (token.kind === Token.LineComment || token.kind === Token.BlockComment) {
      if (out && !token.breaks) out += ' ' + token.text
      else out += (out ? lineBreak(frames.length, token.breaks > 1 && !afterOpen) : '') + token.text
      if (token.kind === Token.LineComment) pending = 'line'
      else if (token.breaks) pending = tokens[i + 1]?.breaks === 0 ? 'space' : 'line'
      afterOpen = false
      continue
    }

    const closing = token.kind === Token.CloseBrace || token.kind === Token.CloseBracket
    if (closing) {
      const frame = frames.pop()
      if (frame && pending !== 'line') pending = frame.inline ? (frame.object && !frame.empty ? 'space' : 'none') : frame.empty ? 'none' : 'line'
    }
    if (out) {
      if (pending === 'line') out += lineBreak(frames.length, token.breaks > 1 && !afterOpen && !closing)
      else if (pending === 'space') out += ' '
    }
    out += token.text
    afterOpen = false

    if (token.kind === Token.OpenBrace || token.kind === Token.OpenBracket) {
      const empty = token.close === i + 1
      const inline = token.close !== undefined && (empty || staysInline(tokens, i))
      const object = token.kind === Token.OpenBrace
      frames.push({ inline, object, empty })
      pending = inline ? (object && !empty ? 'space' : 'none') : empty ? 'none' : 'line'
      afterOpen = true
    } else if (token.kind === Token.Comma) {
      pending = frames.at(-1)?.inline ? 'space' : 'line'
    } else if (token.kind === Token.Colon) {
      pending = 'space'
    } else {
      pending = 'none'
    }
  }
  return out + (text.endsWith('\n') ? '\n' : '')
}

function tomlMeaning(text: string): unknown {
  try {
    return parseToml(text)
  } catch (e) {
    throw e instanceof TomlError ? new InvalidAt(e.line) : e
  }
}

function yamlMeaning(text: string): unknown {
  const documents = parseAllDocuments(text).map((document) => {
    if (document.errors.length) throw new InvalidAt(document.errors[0].linePos?.[0].line ?? null)
    return document.toJS()
  })
  // The yaml library rebuilds the text from its document model, so we also check that it
  // kept every comment.
  const comments = Array.from(new Lexer().lex(text)).filter((token) => token.startsWith('#')).length
  return { documents, comments }
}

function formatYaml(text: string): string {
  const documents = parseAllDocuments(text)
  // A file of only comments has no documents to print, so printing would drop the comments.
  if (!documents.length) return text
  const indent = detectIndent(text, '  ')
  const options = { indent: indent === '\t' ? 2 : indent.length, indentSeq: indentsSequences(text), lineWidth: 0 }
  return documents.map((document) => document.toString(options)).join('')
}

// YAML allows a list under a key at the key's own indentation or deeper, so we keep the
// file's existing style.
function indentsSequences(text: string): boolean {
  const match = text.match(/^( *)[^\s#-][^\n]*:[ \t]*(?:#[^\n]*)?\n(?:[ \t]*(?:#[^\n]*)?\n)*( *)- /m)
  return !match || match[2].length > match[1].length
}

interface TomlPiece {
  kind: 'string' | 'word' | 'punct' | 'comment'
  text: string
  spaced: boolean
}

interface TomlLine {
  text: string
  /** Indent depth, or null for a comment that takes the indent of the line after it. */
  depth: number | null
  verbatim?: boolean
}

interface TomlScan {
  /** The delimiter of a multi-line string still open at the end of the line. */
  openString: string | null
  brackets: number
}

// Forge's config library (NightConfig) indents each table's keys by its depth, so we keep
// that layout in files that already use it and leave flat files flat.
function formatToml(text: string): string {
  const source = text.split('\n')
  const unit = detectIndent(text, '    ')
  const nested = indentsTables(source)
  const scan: TomlScan = { openString: null, brackets: 0 }
  const lines: TomlLine[] = []
  let keyDepth = 0

  for (const raw of source) {
    if (scan.openString) {
      const close = closingDelimiter(raw, 0, scan.openString)
      if (close < 0) {
        lines.push({ text: raw, depth: 0, verbatim: true })
        continue
      }
      scan.openString = null
      const rest = joinToml(tokenizeToml(raw.slice(close), scan))
      lines.push({ text: raw.slice(0, close) + (rest && !/^[,\]}]/.test(rest) ? ' ' : '') + rest, depth: 0, verbatim: true })
      continue
    }
    const content = raw.trimStart()
    if (!content.trim()) {
      if (lines.length && lines.at(-1)!.text !== '') lines.push({ text: '', depth: 0 })
      continue
    }
    const bracketsBefore = scan.brackets
    const pieces = tokenizeToml(content, scan)
    let body = joinToml(pieces)
    if (!scan.openString) body = body.trimEnd()
    const first = pieces[0]

    if (bracketsBefore > 0) {
      const closes = first.kind === 'punct' && (first.text === ']' || first.text === '}')
      lines.push({ text: body, depth: keyDepth + bracketsBefore - (closes ? 1 : 0) })
    } else if (first.kind === 'comment') {
      lines.push({ text: body, depth: null })
    } else if (first.kind === 'punct' && first.text === '[') {
      const parts = pieces.filter((p) => p.kind === 'word').reduce((dots, p) => dots + (p.text.match(/\./g)?.length ?? 0), 1)
      keyDepth = nested ? parts : 0
      lines.push({ text: body, depth: nested ? parts - 1 : 0 })
    } else {
      lines.push({ text: body, depth: keyDepth })
    }
  }

  while (lines.length && lines.at(-1)!.text === '') lines.pop()
  let next = keyDepth
  for (let i = lines.length - 1; i >= 0; i--) {
    const line = lines[i]
    if (line.depth === null) line.depth = next
    else if (line.text !== '' && !line.verbatim) next = line.depth
  }
  const out = lines.map((line) => (line.verbatim || line.text === '' ? line.text : unit.repeat(line.depth!) + line.text)).join('\n')
  return out + (text.endsWith('\n') ? '\n' : '')
}

function indentsTables(lines: string[]): boolean {
  let header = false
  let indented = 0
  let flat = 0
  for (const line of lines) {
    if (/^\s*\[/.test(line)) header = true
    else if (header && /^\s*[\w"'-][^=]*=/.test(line)) {
      if (/^\s/.test(line)) indented++
      else flat++
    }
  }
  return indented > flat
}

function tokenizeToml(line: string, scan: TomlScan): TomlPiece[] {
  const pieces: TomlPiece[] = []
  let spaced = false
  let i = 0
  while (i < line.length) {
    const c = line[i]
    if (c === ' ' || c === '\t') {
      spaced = true
      i++
      continue
    }
    let end: number
    let kind: TomlPiece['kind']
    if (c === '#') {
      kind = 'comment'
      end = line.length
    } else if (line.startsWith('"""', i) || line.startsWith("'''", i)) {
      kind = 'string'
      const delimiter = line.slice(i, i + 3)
      end = closingDelimiter(line, i + 3, delimiter)
      if (end < 0) {
        scan.openString = delimiter
        end = line.length
      }
    } else if (c === '"' || c === "'") {
      kind = 'string'
      end = i + 1
      while (end < line.length && line[end] !== c) end += c === '"' && line[end] === '\\' ? 2 : 1
      end = Math.min(end + 1, line.length)
    } else if ('[]{},='.includes(c)) {
      kind = 'punct'
      end = i + 1
      if (c === '[' || c === '{') scan.brackets++
      if (c === ']' || c === '}') scan.brackets = Math.max(scan.brackets - 1, 0)
    } else {
      kind = 'word'
      end = i + 1
      while (end < line.length && !/[\s[\]{},=#"']/.test(line[end])) end++
    }
    const piece = line.slice(i, end)
    pieces.push({ kind, text: kind === 'comment' ? piece.trimEnd() : piece, spaced })
    spaced = false
    i = end
  }
  return pieces
}

/** Returns the index just past the delimiter that closes a multi-line string, or -1. */
function closingDelimiter(line: string, from: number, delimiter: string): number {
  for (let i = from; i < line.length; i++) {
    if (delimiter === '"""' && line[i] === '\\') {
      i++
      continue
    }
    if (line.startsWith(delimiter, i)) {
      // TOML lets up to two quotes sit right before the closing delimiter as content.
      let end = i + 3
      while (end < line.length && end - i < 5 && line[end] === delimiter[0]) end++
      return end
    }
  }
  return -1
}

function joinToml(pieces: TomlPiece[]): string {
  let out = ''
  pieces.forEach((piece, i) => {
    if (i) out += tomlGap(pieces[i - 1], piece)
    out += piece.text
  })
  return out
}

function tomlGap(before: TomlPiece, after: TomlPiece): string {
  if (after.kind === 'comment') return ' '
  const a = before.kind === 'punct' ? before.text : ''
  const b = after.kind === 'punct' ? after.text : ''
  if (b === ',') return ''
  if (a === ',') return ' '
  if (a === '[' || b === ']') return ''
  if (a === '{') return b === '}' ? '' : ' '
  if (b === '}' || a === '=' || b === '=') return ' '
  return after.spaced ? ' ' : ''
}

// Past this many line pairs, matching unchanged lines costs more than it saves.
const DIFF_CELLS = 2_000_000

/**
 * Returns the smallest edits we can cheaply find that turn `before` into `after`, so the
 * cursor, selections and folds stay where they were.
 */
export function asReleaseType(type: string): string {
  const lower = type.toLowerCase()
  return lower === 'release' ? 'a release' : `${/^[aeiou]/.test(lower) ? 'an' : 'a'} ${lower}`
}

export function changesBetween(before: string, after: string): ChangeSpec[] {
  const a = linesOf(before)
  const b = linesOf(after)
  const changes: ChangeSpec[] = []
  const replace = (from: number, x: string, y: string) => {
    if (x === y) return
    let start = 0
    while (start < x.length && start < y.length && x[start] === y[start]) start++
    let end = 0
    while (end < x.length - start && end < y.length - start && x[x.length - 1 - end] === y[y.length - 1 - end]) end++
    let change = { from: from + start, to: from + x.length - end, insert: y.slice(start, y.length - end) }
    // We split on a newline we add to the end of both texts, so an edit can run past the
    // real end of the document by that one character.
    if (change.from > before.length) change = { from: before.length, to: before.length, insert: '\n' + change.insert.slice(0, -1) }
    else if (change.to > before.length) change = change.from > 0 ? { ...change, from: change.from - 1, to: change.to - 1 } : { ...change, to: before.length }
    changes.push(change)
  }

  let i = 0
  let j = 0
  let offset = 0
  for (const [mi, mj] of [...matchLines(a, b), [a.length, b.length]]) {
    const x = a.slice(i, mi)
    const y = b.slice(j, mj)
    if (x.length === y.length) {
      x.forEach((line, k) => {
        replace(offset, line, y[k])
        offset += line.length
      })
    } else {
      replace(offset, x.join(''), y.join(''))
      offset += x.reduce((sum, line) => sum + line.length, 0)
    }
    offset += a[mi]?.length ?? 0
    i = mi + 1
    j = mj + 1
  }
  return changes
}

function linesOf(text: string): string[] {
  return (text + '\n').split('\n').slice(0, -1).map((line) => line + '\n')
}

function matchLines(a: string[], b: string[]): [number, number][] {
  let start = 0
  while (start < a.length && start < b.length && a[start] === b[start]) start++
  let endA = a.length
  let endB = b.length
  while (endA > start && endB > start && a[endA - 1] === b[endB - 1]) {
    endA--
    endB--
  }
  const pairs: [number, number][] = []
  for (let k = 0; k < start; k++) pairs.push([k, k])
  const n = endA - start
  const m = endB - start
  if (n && m && n * m <= DIFF_CELLS) {
    const width = m + 1
    const common = new Uint16Array((n + 1) * width)
    for (let x = n - 1; x >= 0; x--) {
      for (let y = m - 1; y >= 0; y--) {
        common[x * width + y] = a[start + x] === b[start + y] ? common[(x + 1) * width + y + 1] + 1 : Math.max(common[(x + 1) * width + y], common[x * width + y + 1])
      }
    }
    let x = 0
    let y = 0
    while (x < n && y < m) {
      if (a[start + x] === b[start + y]) pairs.push([start + x++, start + y++])
      else if (common[(x + 1) * width + y] >= common[x * width + y + 1]) x++
      else y++
    }
  }
  for (let k = 0; k < a.length - endA; k++) pairs.push([endA + k, endB + k])
  return pairs
}

// Mirrors publish::release_tag: a version like v4.1.5 is its own tag, and the rest get a v.
export function releaseTag(version: string): string {
  return /^v\d/.test(version) ? version : `v${version}`
}

/** Returns what's wrong with a CurseForge file that waits on you, or null when it waits on CurseForge or nothing. */
export function curseforgeProblem(state: FileState | null, version: string, file: PackFile): string | null {
  const pack = `the ${version} ${file} pack`
  switch (state) {
    case 'manualReview':
      return `CurseForge is holding ${pack} for a moderator`
    case 'changesRequired':
      return `CurseForge asks for changes to ${pack}`
    case 'rejected':
      return `CurseForge rejected ${pack}`
    case 'malwareDetected':
      return `CurseForge flagged ${pack} as malware`
    case 'failedPublishing':
      return `CurseForge couldn't publish ${pack}`
    case 'deleted':
      return `The ${version} ${file} pack is gone from CurseForge`
    default:
      return null
  }
}
