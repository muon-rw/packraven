export interface ScriptLink {
  path: string
  line: number | null
  text: string
  start: number
  end: number
}

// KubeJS logs script errors as "server_scripts:recipes.js#42", "kubejs/server_scripts/recipes.js:42",
// or "server_scripts:mods/Actually Additions/multis.js" (without a line number, and with the spaces
// in pack authors' folder names). The lookahead keeps "data.json" or "a.js.bak" from linking as a
// script, while a sentence's closing period still allows a link.
const SCRIPT_REF = /(?<![A-Za-z0-9_])(?:kubejs[\\/])?(server_scripts|client_scripts|startup_scripts)[:\\/]([^\n:)#"'\]<>]+?\.m?js)(?![A-Za-z0-9_]|\.[A-Za-z0-9_])(?:[:#](\d+))?/gi

// KubeJS's own logs, like logs/kubejs/server.log, start a script's console output with its path
// relative to the log's folder, as in "[08:54:48] [INFO] mods/gtceu/ores.js#149: Done". LogPanel
// strips the timestamp and level header before rendering, so the prefix is optional here; anywhere
// else in a line, a bare "x.js" is too likely to be a fragment of a longer path or plain prose.
const BARE_SCRIPT_REF = /^(?:\[[^\]]*\] \[[A-Z]+\] )?([^:#\n\s][^:#\n]*?\.m?js)#(\d+):/d

export function kubejsLogFolder(path: string): string | null {
  const match = /(?:^|\/)logs\/kubejs\/(server|client|startup)\.(?:log|txt)$/.exec(path.replace(/\\/g, '/'))
  return match ? `${match[1]}_scripts` : null
}

function lineNumber(raw: string | undefined) {
  const line = raw ? Number.parseInt(raw, 10) : null
  return line && line > 0 ? line : null
}

export function findScriptLinks(text: string, scriptFolder: string | null = null): ScriptLink[] {
  const links: ScriptLink[] = []
  SCRIPT_REF.lastIndex = 0
  for (let match = SCRIPT_REF.exec(text); match; match = SCRIPT_REF.exec(text)) {
    const folder = match[1].toLowerCase()
    const file = match[2].replace(/\\/g, '/').replace(/^\/+/, '')
    if (!file || file.includes('..')) continue
    links.push({ path: `kubejs/${folder}/${file}`, line: lineNumber(match[3]), text: match[0], start: match.index, end: match.index + match[0].length })
  }
  const bare = scriptFolder ? BARE_SCRIPT_REF.exec(text) : null
  const at = bare?.indices?.[1]
  const lineSpan = bare?.indices?.[2]
  if (bare && at && lineSpan) {
    const file = bare[1].replace(/^(?:\.\/)+/, '')
    const end = lineSpan[1]
    if (!file.includes('..') && !links.some((link) => at[0] < link.end && end > link.start)) {
      links.push({ path: `kubejs/${scriptFolder}/${file}`, line: lineNumber(bare[2]), text: text.slice(at[0], end), start: at[0], end })
    }
  }
  return links.sort((a, b) => a.start - b.start)
}

export type LogSegment = { text: string; link: ScriptLink | null }

export function splitScriptLinks(text: string, scriptFolder: string | null = null): LogSegment[] {
  const links = findScriptLinks(text, scriptFolder)
  if (!links.length) return [{ text, link: null }]
  const segments: LogSegment[] = []
  let at = 0
  for (const link of links) {
    if (link.start > at) segments.push({ text: text.slice(at, link.start), link: null })
    segments.push({ text: link.text, link })
    at = link.end
  }
  if (at < text.length) segments.push({ text: text.slice(at), link: null })
  return segments
}
