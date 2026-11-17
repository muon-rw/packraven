// A parser for the subset of Markdown Discord renders, producing a tree the preview builds elements
// from. We never produce HTML here, so nothing in a changelog can reach the DOM as markup.

export type MentionKind = 'user' | 'role' | 'channel' | 'everyone' | 'here'
export type TimestampStyle = 't' | 'T' | 'd' | 'D' | 'f' | 'F' | 'R'

export type Inline =
  | { type: 'text'; text: string }
  | { type: 'br' }
  | { type: 'bold' | 'italic' | 'underline' | 'strike' | 'spoiler'; children: Inline[] }
  | { type: 'code'; text: string }
  | { type: 'link'; url: string; children: Inline[] }
  | { type: 'emoji'; name: string; id: string; animated: boolean }
  | { type: 'mention'; kind: MentionKind; id?: string }
  | { type: 'timestamp'; unix: number; style: TimestampStyle }

export type Block =
  | { type: 'paragraph'; children: Inline[] }
  | { type: 'blank' }
  | { type: 'heading'; level: 1 | 2 | 3; children: Inline[] }
  | { type: 'subtext'; children: Inline[] }
  | { type: 'quote'; children: Block[] }
  | { type: 'list'; ordered: boolean; start: number; items: Block[][] }
  | { type: 'codeblock'; language: string; text: string }

export interface InlineOptions {
  // Discord doesn't link anything in an embed title, since the whole title is the link.
  links?: boolean
}

type Rule = (source: string, previous: string, options: InlineOptions) => { node: Inline; length: number } | null

const WORD = /[\p{L}\p{N}_]/u

function wrap(type: 'bold' | 'italic' | 'underline' | 'strike' | 'spoiler', pattern: RegExp, needsBoundary = false): Rule {
  return (source, previous, options) => {
    if (needsBoundary && WORD.test(previous)) return null
    const match = pattern.exec(source)
    if (!match || !match[1].trim()) return null
    return { node: { type, children: parseInline(match[1], options) }, length: match[0].length }
  }
}

export function isSafeUrl(url: string): boolean {
  return /^https?:\/\/[^\s]+$/i.test(url)
}

const rules: Rule[] = [
  (source) => {
    const match = /^\\([^\p{L}\p{N}\s])/u.exec(source)
    return match ? { node: { type: 'text', text: match[1] }, length: 2 } : null
  },
  (source) => {
    const match = /^(`+)([\s\S]*?[^`])\1(?!`)/.exec(source)
    return match ? { node: { type: 'code', text: match[2] }, length: match[0].length } : null
  },
  (source) => {
    const match = /^<(a?):(\w{2,32}):(\d{1,20})>/.exec(source)
    return match ? { node: { type: 'emoji', animated: match[1] === 'a', name: match[2], id: match[3] }, length: match[0].length } : null
  },
  (source) => {
    const match = /^<t:(-?\d{1,13})(?::([tTdDfFR]))?>/.exec(source)
    return match ? { node: { type: 'timestamp', unix: Number(match[1]), style: (match[2] as TimestampStyle) ?? 'f' }, length: match[0].length } : null
  },
  (source) => {
    const match = /^<(@!?|@&|#)(\d{1,20})>/.exec(source)
    if (!match) return null
    const kind: MentionKind = match[1] === '@&' ? 'role' : match[1] === '#' ? 'channel' : 'user'
    return { node: { type: 'mention', kind, id: match[2] }, length: match[0].length }
  },
  (source) => {
    const match = /^@(everyone|here)\b/.exec(source)
    return match ? { node: { type: 'mention', kind: match[1] as MentionKind }, length: match[0].length } : null
  },
  (source, _previous, options) => {
    const match = /^<(https?:\/\/[^\s>]+)>/i.exec(source)
    if (!match) return null
    const text: Inline = { type: 'text', text: match[1] }
    return { node: options.links === false ? text : { type: 'link', url: match[1], children: [text] }, length: match[0].length }
  },
  (source, _previous, options) => {
    if (options.links === false) return null
    const match = /^\[((?:\\.|[^\[\]\\])+)\]\(\s*<?(https?:\/\/[^\s()<>]+(?:\([^\s()<>]*\)[^\s()<>]*)*)>?(?:\s+"[^"\n]*")?\s*\)/i.exec(source)
    if (!match || !match[1].trim()) return null
    return { node: { type: 'link', url: match[2], children: parseInline(match[1], { links: false }) }, length: match[0].length }
  },
  (source, previous, options) => {
    if (WORD.test(previous)) return null
    const match = /^https?:\/\/[^\s<]*[^\s<.,:;"'!?)\]]/i.exec(source)
    if (!match) return null
    const text: Inline = { type: 'text', text: match[0] }
    return { node: options.links === false ? text : { type: 'link', url: match[0], children: [text] }, length: match[0].length }
  },
  wrap('bold', /^\*\*((?:\\[\s\S]|[^\\])+?)\*\*(?!\*)/),
  wrap('underline', /^__((?:\\[\s\S]|[^\\])+?)__(?!_)/),
  wrap('italic', /^\*(?=\S)((?:\*\*|\\[\s\S]|\s+(?:\\[\s\S]|[^\s*\\]|\*\*)|[^\s*\\])+?)\*(?!\*)/),
  wrap('italic', /^_((?:__|\\[\s\S]|[^\\_])+?)_(?![\p{L}\p{N}_])/u, true),
  wrap('strike', /^~~([\s\S]+?)~~/),
  wrap('spoiler', /^\|\|([\s\S]+?)\|\|/),
]

// Plain text runs up to the next character a rule could start on.
const PLAIN = /^[^\\`*_~|<\[@h\n]+/

export function parseInline(source: string, options: InlineOptions = {}): Inline[] {
  const nodes: Inline[] = []
  const pushText = (text: string) => {
    const last = nodes[nodes.length - 1]
    if (last?.type === 'text') last.text += text
    else nodes.push({ type: 'text', text })
  }
  let index = 0
  while (index < source.length) {
    const rest = source.slice(index)
    if (rest[0] === '\n') {
      nodes.push({ type: 'br' })
      index++
      continue
    }
    const previous = index > 0 ? source[index - 1] : ''
    let matched = false
    for (const rule of rules) {
      const result = rule(rest, previous, options)
      if (result) {
        if (result.node.type === 'text') pushText(result.node.text)
        else nodes.push(result.node)
        index += result.length
        matched = true
        break
      }
    }
    if (matched) continue
    const plain = PLAIN.exec(rest)
    const length = plain ? plain[0].length : 1
    pushText(rest.slice(0, length))
    index += length
  }
  return nodes
}

const HEADING = /^(#{1,3}) +(\S.*)$/
const SUBTEXT = /^-# +(\S.*)$/
const LIST_ITEM = /^( *)([-*]|\d{1,9}\.) +(\S.*)$/
const isQuoteLine = (line: string) => line.startsWith('> ') || line === '>'
const FENCE = /^```(?:([\w+#.-]+)\n)?\n?([\s\S]*?)\n?```/

interface ListLine {
  indent: number
  ordered: boolean
  start: number
  text: string
}

function buildList(lines: ListLine[]): Block {
  const base = lines[0]
  const items: Block[][] = []
  let index = 0
  while (index < lines.length) {
    const line = lines[index]
    const nested: ListLine[] = []
    index++
    while (index < lines.length && lines[index].indent > base.indent) nested.push(lines[index++])
    const item: Block[] = [{ type: 'paragraph', children: parseInline(line.text) }]
    if (nested.length) item.push(buildList(nested))
    items.push(item)
  }
  return { type: 'list', ordered: base.ordered, start: base.start, items }
}

export function parseBlocks(source: string, { inQuote = false } = {}): Block[] {
  const blocks: Block[] = []
  const lines = source.replace(/\r\n?/g, '\n').split('\n')
  let paragraph: string[] = []

  const flushParagraph = () => {
    let leading = 0
    while (leading < paragraph.length && !paragraph[leading].trim()) leading++
    let trailing = paragraph.length
    while (trailing > leading && !paragraph[trailing - 1].trim()) trailing--
    for (let i = 0; i < leading; i++) blocks.push({ type: 'blank' })
    if (trailing > leading) blocks.push({ type: 'paragraph', children: parseInline(paragraph.slice(leading, trailing).join('\n')) })
    for (let i = trailing; i < paragraph.length; i++) blocks.push({ type: 'blank' })
    paragraph = []
  }

  let index = 0
  while (index < lines.length) {
    const line = lines[index]
    if (line.startsWith('```')) {
      const fence = FENCE.exec(lines.slice(index).join('\n'))
      if (fence) {
        flushParagraph()
        blocks.push({ type: 'codeblock', language: fence[1] ?? '', text: fence[2] })
        const consumed = fence[0].split('\n').length
        const after = lines[index + consumed - 1].slice(lines[index + consumed - 1].lastIndexOf('```') + 3)
        index += consumed
        if (after.trim()) lines.splice(index, 0, after)
        continue
      }
    }
    if (!inQuote && line.startsWith('>>> ')) {
      flushParagraph()
      blocks.push({ type: 'quote', children: parseBlocks(lines.slice(index).join('\n').slice(4), { inQuote: true }) })
      break
    }
    if (!inQuote && isQuoteLine(line)) {
      flushParagraph()
      const quoted: string[] = []
      while (index < lines.length && isQuoteLine(lines[index]) && !lines[index].startsWith('>>> ')) quoted.push(lines[index++].slice(2))
      blocks.push({ type: 'quote', children: parseBlocks(quoted.join('\n'), { inQuote: true }) })
      continue
    }
    const heading = HEADING.exec(line)
    if (heading) {
      flushParagraph()
      blocks.push({ type: 'heading', level: heading[1].length as 1 | 2 | 3, children: parseInline(heading[2]) })
      index++
      continue
    }
    const subtext = SUBTEXT.exec(line)
    if (subtext) {
      flushParagraph()
      blocks.push({ type: 'subtext', children: parseInline(subtext[1]) })
      index++
      continue
    }
    if (LIST_ITEM.test(line)) {
      flushParagraph()
      const items: ListLine[] = []
      let match: RegExpExecArray | null
      while (index < lines.length && (match = LIST_ITEM.exec(lines[index]))) {
        const ordered = /\d/.test(match[2])
        items.push({ indent: match[1].length, ordered, start: ordered ? parseInt(match[2], 10) : 1, text: match[3] })
        index++
      }
      // An item indented further than the first starts a nested list, so the first sets the base.
      const base = items[0].indent
      blocks.push(buildList(items.map((item) => ({ ...item, indent: Math.max(item.indent, base) }))))
      continue
    }
    paragraph.push(line)
    index++
  }
  flushParagraph()
  return blocks
}

const EMOJI_ONLY_LIMIT = 30

// Discord shows emoji larger when a message has nothing else in it.
export function isEmojiOnly(blocks: Block[]): boolean {
  if (blocks.length !== 1 || blocks[0].type !== 'paragraph') return false
  let count = 0
  for (const node of blocks[0].children) {
    if (node.type === 'emoji') count++
    else if (node.type === 'text') {
      const stripped = node.text.replace(/\s+/g, '')
      if (!stripped) continue
      const emoji = stripped.match(/\p{Extended_Pictographic}/gu) ?? []
      if (stripped.replace(/[\p{Extended_Pictographic}\p{Emoji_Modifier}‍️⃣]/gu, '')) return false
      count += emoji.length
    } else if (node.type !== 'br') return false
  }
  return count > 0 && count <= EMOJI_ONLY_LIMIT
}

export function emojiUrl(emoji: { id: string; animated: boolean }): string {
  return `https://cdn.discordapp.com/emojis/${emoji.id}.${emoji.animated ? 'gif' : 'webp'}?size=48`
}

export interface TimeOptions {
  now?: Date
  locale?: string
  timeZone?: string
}

const TIMESTAMP_FORMATS: Record<Exclude<TimestampStyle, 'R'>, Intl.DateTimeFormatOptions> = {
  t: { hour: 'numeric', minute: '2-digit' },
  T: { hour: 'numeric', minute: '2-digit', second: '2-digit' },
  d: { year: 'numeric', month: '2-digit', day: '2-digit' },
  D: { year: 'numeric', month: 'long', day: 'numeric' },
  f: { year: 'numeric', month: 'long', day: 'numeric', hour: 'numeric', minute: '2-digit' },
  F: { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric', hour: 'numeric', minute: '2-digit' },
}

const RELATIVE_UNITS: [Intl.RelativeTimeFormatUnit, number][] = [
  ['year', 365 * 24 * 3600],
  ['month', 30 * 24 * 3600],
  ['day', 24 * 3600],
  ['hour', 3600],
  ['minute', 60],
  ['second', 1],
]

export function formatTimestamp(unix: number, style: TimestampStyle, { now = new Date(), locale, timeZone }: TimeOptions = {}): string {
  const date = new Date(unix * 1000)
  if (Number.isNaN(date.getTime())) return 'Invalid Date'
  if (style === 'R') {
    const seconds = Math.round((date.getTime() - now.getTime()) / 1000)
    const [unit, size] = RELATIVE_UNITS.find(([, size]) => Math.abs(seconds) >= size) ?? ['second', 1]
    return new Intl.RelativeTimeFormat(locale, { numeric: 'always' }).format(Math.trunc(seconds / size), unit)
  }
  return new Intl.DateTimeFormat(locale, { ...TIMESTAMP_FORMATS[style], timeZone }).format(date)
}

function dayKey(date: Date, timeZone?: string) {
  return new Intl.DateTimeFormat('en-CA', { timeZone, year: 'numeric', month: '2-digit', day: '2-digit' }).format(date)
}

// Discord's own format for message and embed times: "Today at 3:04 PM", or the date for older ones.
export function formatMessageTime(date: Date, { now = new Date(), locale, timeZone }: TimeOptions = {}): string {
  const time = new Intl.DateTimeFormat(locale, { hour: 'numeric', minute: '2-digit', timeZone }).format(date)
  const yesterday = new Date(now.getTime() - 24 * 3600 * 1000)
  if (dayKey(date, timeZone) === dayKey(now, timeZone)) return `Today at ${time}`
  if (dayKey(date, timeZone) === dayKey(yesterday, timeZone)) return `Yesterday at ${time}`
  return `${new Intl.DateTimeFormat(locale, { year: 'numeric', month: '2-digit', day: '2-digit', timeZone }).format(date)} ${time}`
}
