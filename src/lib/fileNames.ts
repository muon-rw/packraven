const PART_START = /[-_+ ]/
const PART_END = /[-_+ .]/
const SEPARATOR_EDGES = /^[-_+ .]+|[-_+ .]+$/g

// Two files of one mod usually share a long name, so we keep only the part of each that differs,
// cut at a separator: Apotheosis-1.21.1-8.1.1.jar and Apotheosis-1.21.1-8.1.2.jar become 8.1.1 and
// 8.1.2. The backend trims changelog lines the same way (changed_parts in changelog.rs).
function changedSpans(from: string, to: string): [string, string] {
  let start = 0
  while (start < from.length && from[start] === to[start]) start++
  while (start > 0 && !PART_START.test(to[start - 1])) start--
  let end = 0
  while (end < from.length - start && end < to.length - start && from[from.length - 1 - end] === to[to.length - 1 - end]) end++
  while (end > 0 && !PART_END.test(to[to.length - end])) end--
  return [from.slice(start, from.length - end).replace(SEPARATOR_EDGES, ''), to.slice(start, to.length - end).replace(SEPARATOR_EDGES, '')]
}

export function changedPart(from: string, to: string) {
  const part = changedSpans(from, to)[1]
  return part.length >= 2 ? part : to
}

export function changedParts(from: string, to: string): [string, string] {
  const [old, next] = changedSpans(from, to)
  return old.length >= 2 && next.length >= 2 ? [old, next] : [from, to]
}
