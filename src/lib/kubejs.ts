import type { Text } from '@codemirror/state'
import type { Diagnostic } from '@codemirror/lint'
import type { KubejsContext } from './api'

export type KubejsSide = 'server' | 'client' | 'startup'

type AstNode = {
  type?: string
  start?: number
  end?: number
  [key: string]: unknown
}

// Without ProbeJS typings, we only know where KubeJS's own event groups run.
const GROUP_SIDES: Record<string, KubejsSide> = {
  ServerEvents: 'server',
  ClientEvents: 'client',
  JEIEvents: 'client',
  REIEvents: 'client',
  EMIEvents: 'client',
  RecipeViewerEvents: 'client',
  StartupEvents: 'startup',
}
const EVENT_SIDES: Record<string, KubejsSide[]> = {
  'ClientEvents.init': ['startup'],
}

function eventSides(group: string, event: string | null, context: KubejsContext | null): KubejsSide[] | null {
  const typed = context?.eventSides ?? {}
  if (event && typed[`${group}.${event}`]) return typed[`${group}.${event}`]
  // ProbeJS knows this group but not this event, so we have nothing reliable to say about it.
  if (Object.keys(typed).some((key) => key.startsWith(`${group}.`))) return null
  if (event && EVENT_SIDES[`${group}.${event}`]) return EVENT_SIDES[`${group}.${event}`]
  return GROUP_SIDES[group] ? [GROUP_SIDES[group]] : null
}

function folderList(sides: KubejsSide[]) {
  const folders = sides.map((side) => `kubejs/${side}_scripts/`)
  return folders.length > 1 ? `${folders.slice(0, -1).join(', ')} and ${folders[folders.length - 1]}` : folders[0]
}

const PRIORITY_VALUE = /^-?\d+$/
const MOD_ID_VALUE = /^[a-z0-9_.-]+$/
const SINGLE_WORD_VALUE = /^\S+$/

// "ignore" and "require" are ordinary words in header comments, so we only suggest the directive
// when the value already looks like one.
const DIRECTIVE_TYPOS: Record<string, { fix: string; value: RegExp }> = {
  ignore: { fix: 'ignored', value: /^(true|false)$/ },
  priorty: { fix: 'priority', value: PRIORITY_VALUE },
  prioriy: { fix: 'priority', value: PRIORITY_VALUE },
  prority: { fix: 'priority', value: PRIORITY_VALUE },
  require: { fix: 'requires', value: MOD_ID_VALUE },
  pack_mode: { fix: 'packmode', value: SINGLE_WORD_VALUE },
  'pack-mode': { fix: 'packmode', value: SINGLE_WORD_VALUE },
}

export function isKubejsScript(path: string): boolean {
  return /^kubejs\/.+\.m?js$/i.test(path.replace(/\\/g, '/'))
}

export function kubejsScriptSide(path: string): KubejsSide | null {
  const match = /^kubejs\/(server|client|startup)_scripts\//i.exec(path.replace(/\\/g, '/'))
  return match ? (match[1].toLowerCase() as KubejsSide) : null
}

export function kubejsScriptDir(path: string): string | null {
  const normalized = path.replace(/\\/g, '/')
  const side = /^(kubejs\/(?:server|client|startup)_scripts)(?:\/|$)/i.exec(normalized)
  if (side) return side[1]
  return /^kubejs(?:\/|$)/i.test(normalized) ? 'kubejs' : null
}

function clamp(doc: Text, from: number, to: number): { from: number; to: number } {
  const start = Math.min(Math.max(from, 0), doc.length)
  return { from: start, to: Math.min(Math.max(to, start), doc.length) }
}

function checkDirectives(doc: Text): Diagnostic[] {
  const diagnostics: Diagnostic[] = []
  for (let n = 1; n <= doc.lines; n++) {
    const line = doc.line(n)
    const trimmed = line.text.trim()
    if (!trimmed) continue
    if (!trimmed.startsWith('//')) break
    const match = /^\/\/\s*([a-zA-Z_-]+)\s*:(.*)$/.exec(trimmed)
    if (!match) continue
    const rawKey = match[1]
    const key = rawKey.toLowerCase()
    const value = match[2].trim()
    const keyStart = line.from + line.text.indexOf(rawKey)
    const keyEnd = keyStart + rawKey.length

    const typo = DIRECTIVE_TYPOS[key]
    if (typo) {
      if (typo.value.test(value)) {
        diagnostics.push({
          ...clamp(doc, keyStart, keyEnd),
          severity: 'warning',
          message: `Did you mean "// ${typo.fix}:"? KubeJS won't recognize "// ${rawKey}:".`,
        })
      }
      continue
    }

    if (key === 'priority') {
      if (!PRIORITY_VALUE.test(value)) {
        diagnostics.push({
          ...clamp(doc, keyStart, line.to),
          severity: 'warning',
          message: '"// priority:" expects a whole number, such as "// priority: 10".',
        })
      }
    } else if (key === 'ignored') {
      if (value !== 'true' && value !== 'false') {
        diagnostics.push({
          ...clamp(doc, keyStart, line.to),
          severity: 'warning',
          message: '"// ignored:" expects true or false.',
        })
      }
    } else if (key === 'requires') {
      if (!value) {
        diagnostics.push({
          ...clamp(doc, keyStart, line.to),
          severity: 'warning',
          message: '"// requires:" needs a mod ID after the colon.',
        })
      }
    } else if (key === 'packmode') {
      if (!value) {
        diagnostics.push({
          ...clamp(doc, keyStart, line.to),
          severity: 'warning',
          message: '"// packmode:" needs a pack mode name after the colon.',
        })
      }
    }
  }
  return diagnostics
}

function collectPatternNames(node: AstNode | null | undefined, out: Set<string>) {
  if (!node || typeof node !== 'object') return
  switch (node.type) {
    case 'Identifier':
      if (typeof node.name === 'string') out.add(node.name)
      break
    case 'AssignmentPattern':
      collectPatternNames(node.left as AstNode, out)
      break
    case 'RestElement':
      collectPatternNames(node.argument as AstNode, out)
      break
    case 'ArrayPattern':
      if (Array.isArray(node.elements)) {
        for (const el of node.elements) collectPatternNames(el as AstNode, out)
      }
      break
    case 'ObjectPattern':
      if (Array.isArray(node.properties)) {
        for (const prop of node.properties as AstNode[]) {
          collectPatternNames((prop.type === 'RestElement' ? prop.argument : prop.value) as AstNode, out)
        }
      }
      break
  }
}

function collectDeclaredNames(stmts: unknown, out: Set<string>) {
  if (!Array.isArray(stmts)) return
  for (const stmt of stmts as AstNode[]) {
    if (!stmt || typeof stmt !== 'object') continue
    if (stmt.type === 'VariableDeclaration' && Array.isArray(stmt.declarations)) {
      for (const decl of stmt.declarations as AstNode[]) collectPatternNames(decl.id as AstNode, out)
    } else if ((stmt.type === 'FunctionDeclaration' || stmt.type === 'ClassDeclaration') && stmt.id) {
      collectPatternNames(stmt.id as AstNode, out)
    }
  }
}

export function checkKubejsAst(doc: Text, ast: AstNode, path: string, context: KubejsContext | null): Diagnostic[] {
  const diagnostics: Diagnostic[] = [...checkDirectives(doc)]
  const side = kubejsScriptSide(path)
  const scopes: Set<string>[] = []

  const isShadowed = (name: string) => scopes.some((s) => s.has(name))

  const push = (from: number, to: number, severity: 'error' | 'warning', message: string) => {
    diagnostics.push({ ...clamp(doc, from, Math.max(to, from + 1)), severity, message })
  }

  const visit = (node: unknown) => {
    if (!node || typeof node !== 'object') return
    if (Array.isArray(node)) {
      for (const item of node) visit(item)
      return
    }
    const n = node as AstNode
    if (typeof n.type !== 'string') return

    const start = typeof n.start === 'number' ? n.start : 0
    const end = typeof n.end === 'number' ? n.end : start + 1

    switch (n.type) {
      case 'ImportDeclaration':
      case 'ExportNamedDeclaration':
      case 'ExportDefaultDeclaration':
      case 'ExportAllDeclaration':
      case 'ImportExpression':
        push(start, end, 'error', "KubeJS runs scripts with Rhino, which doesn't support ES module import/export syntax.")
        break
      case 'AwaitExpression':
        push(start, Math.min(start + 5, end), 'error', "Rhino doesn't support async/await in KubeJS scripts.")
        break
      case 'PrivateIdentifier':
        push(start, end, 'error', "Rhino doesn't support private class fields (#name).")
        break
      case 'StaticBlock':
        push(start, Math.min(start + 6, end), 'error', "Rhino doesn't support class static blocks.")
        break
      case 'Literal':
        if (n.bigint !== undefined) {
          push(start, end, 'error', "Rhino doesn't support BigInt literals.")
        } else if (typeof n.value === 'number' && typeof n.raw === 'string' && n.raw.includes('_')) {
          push(start, end, 'warning', 'Older Rhino builds reject numeric separators like 1_000.')
        }
        break
      case 'FunctionDeclaration':
      case 'FunctionExpression':
      case 'ArrowFunctionExpression': {
        if (n.async === true) {
          push(start, Math.min(start + 5, end), 'error', "Rhino doesn't support async/await in KubeJS scripts.")
        }
        break
      }
      case 'MemberExpression': {
        const obj = n.object as AstNode | undefined
        if (side && obj?.type === 'Identifier' && typeof obj.name === 'string' && !isShadowed(obj.name)) {
          const property = n.property as AstNode | undefined
          const event = !n.computed && property?.type === 'Identifier' && typeof property.name === 'string' ? property.name : null
          const sides = eventSides(obj.name, event, context)
          if (sides && !sides.includes(side)) {
            push(
              typeof obj.start === 'number' ? obj.start : start,
              typeof property?.end === 'number' && event ? property.end : typeof obj.end === 'number' ? obj.end : end,
              'warning',
              `${event ? `${obj.name}.${event}` : obj.name} only runs in ${folderList(sides)}.`,
            )
          }
        }
        break
      }
    }

    const pushesScope = n.type === 'Program' || n.type === 'BlockStatement' || n.type === 'FunctionDeclaration' || n.type === 'FunctionExpression' || n.type === 'ArrowFunctionExpression' || n.type === 'CatchClause'
    if (pushesScope) {
      const scope = new Set<string>()
      if (n.type === 'Program' || n.type === 'BlockStatement') {
        collectDeclaredNames(n.body, scope)
      } else if (n.type === 'CatchClause') {
        collectPatternNames(n.param as AstNode, scope)
      } else {
        if (n.id) collectPatternNames(n.id as AstNode, scope)
        if (Array.isArray(n.params)) {
          for (const param of n.params as AstNode[]) collectPatternNames(param, scope)
        }
      }
      scopes.push(scope)
    }

    for (const [key, value] of Object.entries(n)) {
      if (key === 'type' || key === 'start' || key === 'end' || key === 'loc' || key === 'range' || key === 'raw' || key === 'comments') continue
      if (value && typeof value === 'object') visit(value)
    }

    if (pushesScope) scopes.pop()
  }

  visit(ast)
  return diagnostics
}
