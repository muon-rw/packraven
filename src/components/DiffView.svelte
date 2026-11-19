<script lang="ts">
  import { tick, untrack } from 'svelte'
  import { SvelteSet } from 'svelte/reactivity'
  import type { LineAction } from '../lib/api'
  import { diffRows, isChange, type DiffRow } from '../lib/diffRows'
  import { BACK_PRIORITY, useBack } from '../lib/back.svelte'
  import { paneFocus } from '../lib/panes.svelte'
  import { commandKey, isMac, shortcut } from '../lib/platform'
  import Notice from './Notice.svelte'

  let {
    text,
    path,
    staged,
    selectable,
    canDiscard,
    onapply,
  }: { text: string; path: string; staged: boolean; selectable: boolean; canDiscard: boolean; onapply: (action: LineAction, lines: number[]) => Promise<boolean> } = $props()

  const STATUS_MS = 8000

  // Selected lines are raw diff line indices, and the cursor and anchor are positions in rows.
  const picked = new SvelteSet<number>()
  let cursor = $state<number | null>(null)
  let anchor: number | null = null
  let anchorBase = new Set<number>()
  let applying = $state(false)
  let status = $state('')
  let failure = $state<string | null>(null)
  let body = $state<HTMLElement>()
  let viewWidth = $state(0)

  let parsed = $derived(diffRows(text))
  let rows = $derived(parsed.rows)
  // The cursor stops on block headers and changed lines, and skips unchanged ones.
  let stops = $derived(rows.flatMap((row, pos) => (isChange(row) || row.kind === 'hunk' ? [pos] : [])))
  let hasChanges = $derived(rows.some(isChange))
  let digits = $derived(String(parsed.lastLine).length)
  let primary = $derived<LineAction>(staged ? 'unstage' : 'stage')
  let verb = $derived(staged ? 'Unstage' : 'Stage')
  let discardable = $derived(!staged && canDiscard)
  let lineWord = $derived(picked.size === 1 ? 'line' : 'lines')

  // A new diff renumbers its lines, so we drop the old selection but keep the cursor where it
  // was, so you can stage one block and carry on with the next.
  $effect(() => {
    void text
    untrack(() => {
      picked.clear()
      anchor = null
      failure = null
      if (cursor !== null) cursor = nearestStop(cursor)
    })
  })

  $effect(() => {
    if (!status) return
    const timer = setTimeout(() => (status = ''), STATUS_MS)
    return () => clearTimeout(timer)
  })

  function nearestStop(pos: number) {
    if (!stops.length) return null
    return stops.find((stop) => stop >= pos) ?? stops[stops.length - 1]
  }

  function setAnchor(pos: number) {
    anchor = pos
    anchorBase = new Set(picked)
  }

  // Shift selects every change between the anchor and `to`, on top of what was selected when the
  // anchor was set, so moving back toward the anchor deselects lines again.
  function extendTo(to: number, value = true) {
    if (anchor === null) setAnchor(to)
    const [from, end] = anchor! < to ? [anchor!, to] : [to, anchor!]
    picked.clear()
    for (const index of anchorBase) picked.add(index)
    for (let pos = from; pos <= end; pos++) {
      if (!isChange(rows[pos])) continue
      if (value) picked.add(rows[pos].index)
      else picked.delete(rows[pos].index)
    }
  }

  let linesByHunk = $derived.by(() => {
    const byHunk = new Map<number, number[]>()
    for (const row of rows) {
      if (!isChange(row)) continue
      if (!byHunk.has(row.hunk)) byHunk.set(row.hunk, [])
      byHunk.get(row.hunk)!.push(row.index)
    }
    return byHunk
  })

  function hunkLines(hunk: number) {
    return linesByHunk.get(hunk) ?? []
  }

  function allPicked(lines: number[]) {
    return lines.length > 0 && lines.every((index) => picked.has(index))
  }

  function toggleAt(pos: number) {
    const row = rows[pos]
    if (row.kind === 'hunk') {
      const lines = hunkLines(row.hunk)
      const all = allPicked(lines)
      for (const index of lines) {
        if (all) picked.delete(index)
        else picked.add(index)
      }
    } else if (picked.has(row.index)) picked.delete(row.index)
    else picked.add(row.index)
    setAnchor(pos)
  }

  // With nothing selected, the keyboard acts on the line or block under the cursor.
  function linesAtCursor() {
    if (picked.size) return [...picked]
    if (cursor === null) return []
    const row = rows[cursor]
    return row.kind === 'hunk' ? hunkLines(row.hunk) : isChange(row) ? [row.index] : []
  }

  function clear() {
    picked.clear()
    anchor = null
    body?.focus({ preventScroll: true })
  }

  const done: Record<LineAction, string> = { stage: 'Staged', unstage: 'Unstaged', discard: 'Discarded' }

  function outcome(action: LineAction, count: number) {
    const lines = `${count} ${count === 1 ? 'line' : 'lines'}`
    if (action === 'stage') return `Staged ${lines}. The file now shows under Staged too, and anything you left shows under Changes.`
    if (action === 'unstage') return `Unstaged ${lines}. They show under Changes again.`
    return `${done[action]} ${lines}.`
  }

  async function apply(action: LineAction, lines: number[]) {
    if (!lines.length || applying) return
    applying = true
    failure = null
    try {
      const applied = await onapply(
        action,
        lines.sort((a, b) => a - b),
      )
      if (applied) status = outcome(action, lines.length)
    } catch (e) {
      failure = String(e)
    } finally {
      applying = false
      body?.focus({ preventScroll: true })
    }
  }

  // Touch captures the pointer to the line you started on, so we find the row under the finger
  // by its position rather than by the event's target.
  function positionAt(x: number, y: number) {
    const row = document.elementFromPoint(x, y)?.closest<HTMLElement>('.diff-body [data-pos]')
    return row ? Number(row.dataset.pos) : null
  }

  // Dragging down the line numbers sets every line it crosses the way the first click set its line.
  function onGutterDown(event: PointerEvent, pos: number) {
    if (event.button !== 0 || !selectable) return
    event.preventDefault()
    body?.focus({ preventScroll: true })
    status = ''
    cursor = pos
    if (event.shiftKey && anchor !== null) return extendTo(pos)
    const value = !picked.has(rows[pos].index)
    setAnchor(pos)
    extendTo(pos, value)
    let last = pos
    const move = (e: PointerEvent) => {
      const to = positionAt(e.clientX, e.clientY)
      if (to === null || to === last || rows[to].kind === 'hunk') return
      last = to
      extendTo(to, value)
      if (isChange(rows[to])) cursor = to
    }
    const end = () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', end)
      window.removeEventListener('pointercancel', end)
    }
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', end)
    window.addEventListener('pointercancel', end)
  }

  function onHunkClick(pos: number) {
    if (!selectable) return
    status = ''
    cursor = pos
    toggleAt(pos)
    body?.focus({ preventScroll: true })
  }

  // A click in the code moves the cursor there, unless you were selecting text to copy.
  function onLineClick(pos: number) {
    if (selectable && isChange(rows[pos]) && window.getSelection()?.isCollapsed !== false) cursor = pos
  }

  async function moveTo(next: number, extend: boolean) {
    if (extend) {
      if (anchor === null) setAnchor(cursor ?? next)
      extendTo(next)
    }
    cursor = next
    await tick()
    document.getElementById(lineId(next))?.scrollIntoView({ block: 'nearest' })
  }

  function onKey(event: KeyboardEvent) {
    if (!selectable || !stops.length) return
    const at = cursor === null ? -1 : stops.indexOf(cursor)
    const clampStop = (i: number) => stops[Math.max(0, Math.min(i, stops.length - 1))]
    const hunkStops = stops.filter((pos) => rows[pos].kind === 'hunk')
    if (event.altKey && event.key === 'ArrowDown') moveTo(hunkStops.find((pos) => pos > (cursor ?? -1)) ?? hunkStops[hunkStops.length - 1], false)
    else if (event.altKey && event.key === 'ArrowUp') moveTo(hunkStops.findLast((pos) => pos < (cursor ?? Infinity)) ?? hunkStops[0], false)
    else if (event.key === 'ArrowDown') moveTo(clampStop(at + 1), event.shiftKey)
    else if (event.key === 'ArrowUp') moveTo(clampStop(at < 0 ? 0 : at - 1), event.shiftKey)
    else if (event.key === 'Home' && commandKey(event)) moveTo(stops[0], event.shiftKey)
    else if (event.key === 'End' && commandKey(event)) moveTo(stops[stops.length - 1], event.shiftKey)
    else if (event.key === ' ') {
      status = ''
      if (cursor === null) moveTo(stops[0], false)
      toggleAt(cursor ?? stops[0])
    } else if (commandKey(event) && event.key.toLowerCase() === 'a') {
      for (const row of rows) if (isChange(row)) picked.add(row.index)
    } else if (event.key === 'Escape' && picked.size) clear()
    else if (event.key === 'Enter') apply(primary, linesAtCursor())
    else if ((event.key === 'Delete' || (isMac && event.metaKey && event.key === 'Backspace')) && discardable) apply('discard', linesAtCursor())
    else return
    event.preventDefault()
  }

  function lineId(pos: number) {
    return `diff-line-${pos}`
  }

  function rowLabel(row: DiffRow) {
    if (row.kind === 'add') return `Added, line ${row.newNo}:`
    if (row.kind === 'del') return `Removed, line ${row.oldNo}:`
    if (row.kind === 'context') return `Unchanged, line ${row.newNo}:`
    return ''
  }

  let keyHelp = $derived(
    `Arrow keys move between changed lines and blocks, and Alt with an arrow jumps to the next block. Space selects. Enter ${staged ? 'unstages' : 'stages'} the selection, or the line or block under the cursor${discardable ? '; Delete discards it' : ''}. Escape clears the selection.`,
  )

  const inFocusedPane = paneFocus()

  useBack(BACK_PRIORITY.selection, () => {
    if (!inFocusedPane() || !picked.size) return false
    clear()
    return true
  })
</script>

{#if selectable && hasChanges}
  <div class="lines-bar">
    <p class="slot" class:off={picked.size > 0}>
      <span class="status" role="status">{status}</span>
      {#if !status}
        <span class="muted">
          Click line numbers to select lines, or use ↑ ↓ and Space. {shortcut('Enter')}
          {staged ? 'unstages' : 'stages'} them.{#if !staged && !canDiscard}&nbsp;To undo mod changes, discard the whole file.{/if}
        </span>
      {/if}
    </p>
    <div class="slot picking" class:off={!picked.size}>
      <span class="picked-count">{picked.size} {lineWord} selected</span>
      <span class="bar-actions">
        <button class="primary" class:busy={applying} disabled={applying} title="{verb} the selected lines ({shortcut('Enter')})" onclick={() => apply(primary, [...picked])}>{verb} {lineWord}</button>
        {#if discardable}<button class="danger" disabled={applying} title="Discard the selected lines ({shortcut('Delete')})" onclick={() => apply('discard', [...picked])}>Discard {lineWord}…</button>{/if}
        <button class="ghost" title="Clear the selection (Esc)" onclick={clear}>Clear</button>
      </span>
    </div>
  </div>
  <span class="visually-hidden" id="diff-keys">{keyHelp}</span>
{/if}
{#if failure}
  <div class="failure"><Notice tone="error"><p>{failure}</p></Notice></div>
{/if}

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
  bind:this={body}
  bind:clientWidth={viewWidth}
  class="diff-body"
  role={selectable ? 'listbox' : 'region'}
  aria-multiselectable={selectable ? 'true' : undefined}
  aria-label="Changes in {path}"
  aria-describedby={selectable && hasChanges ? 'diff-keys' : undefined}
  aria-activedescendant={cursor !== null ? lineId(cursor) : undefined}
  tabindex="0"
  onkeydown={onKey}
  style:--view={viewWidth ? `${viewWidth}px` : '100%'}
>
  <div class="sheet" style:--digits={digits} style:--cols={parsed.width}>
    {#each rows as row, pos (row.index)}
      {#if row.kind === 'hunk'}
        {@const lines = hunkLines(row.hunk)}
        {@const all = allPicked(lines)}
        <!-- svelte-ignore a11y_click_events_have_key_events (the listbox handles keys for its options) -->
        <div
          id={lineId(pos)}
          class="line hunk"
          class:pickable={selectable}
          class:picked={all}
          class:cursor={cursor === pos}
          data-pos={pos}
          role={selectable ? 'option' : undefined}
          aria-selected={selectable ? all : undefined}
          aria-label="Block, {row.range?.toLowerCase()}{row.text ? `, in ${row.text.trim()}` : ''}, {lines.length} {lines.length === 1 ? 'change' : 'changes'}"
          title={selectable ? (all ? 'Deselect this block' : 'Select every change in this block') : undefined}
          onclick={() => onHunkClick(pos)}
        >
          <span class="range">{row.range}</span>
          <span class="context">{row.text}</span>
          {#if selectable && lines.length}
            <!-- Hidden from screen readers, since an option can't hold buttons; Enter and Delete on the block cover them. -->
            <span class="hunk-actions" aria-hidden="true">
              <button class="ghost" tabindex="-1" disabled={applying} title="{verb} every change in this block" onclick={(e) => (e.stopPropagation(), apply(primary, lines))}>{verb}<span class="wide">&nbsp;block</span></button>
              {#if discardable}
                <button class="ghost danger-text" tabindex="-1" disabled={applying} title="Discard every change in this block" onclick={(e) => (e.stopPropagation(), apply('discard', lines))}>Discard<span class="wide">&nbsp;block</span>…</button>
              {/if}
            </span>
          {/if}
        </div>
      {:else}
        {@const change = isChange(row)}
        <!-- svelte-ignore a11y_click_events_have_key_events (the listbox handles keys for its options) -->
        <div
          id={lineId(pos)}
          class="line {row.kind}"
          class:picked={picked.has(row.index)}
          class:cursor={cursor === pos}
          data-pos={pos}
          role={selectable ? 'option' : undefined}
          aria-selected={selectable && change ? picked.has(row.index) : undefined}
          aria-disabled={selectable && !change ? 'true' : undefined}
          onclick={() => onLineClick(pos)}
        >
          <!-- svelte-ignore a11y_no_static_element_interactions (keyboard users select lines with the arrow keys and Space) -->
          <span class="gutter" class:pickable={selectable && change} onpointerdown={(e) => change && onGutterDown(e, pos)}>
            <span class="no" aria-hidden="true">{row.oldNo ?? ''}</span><span class="no" aria-hidden="true">{row.newNo ?? ''}</span><span class="sign" aria-hidden="true"
              >{row.kind === 'add' ? '+' : row.kind === 'del' ? '−' : ''}</span
            >{#if rowLabel(row)}<span class="visually-hidden">{rowLabel(row)}</span>{/if}
          </span>
          <span class="code"
            >{#if row.changed}{row.text.slice(0, row.changed[0])}<mark>{row.text.slice(row.changed[0], row.changed[1])}</mark>{row.text.slice(row.changed[1])}{:else}{row.text || ' '}{/if}{#if row.noNewline}<span class="eol"
                >No newline at end of file</span
              >{/if}</span
          >
        </div>
      {/if}
    {/each}
  </div>
</div>

<style>
  /* Both states share one grid cell, so selecting a line never moves the diff under your pointer. */
  .lines-bar {
    display: grid;
    align-items: center;
    padding: 6px 10px 6px 14px;
    font-size: var(--step--1);
    border-bottom: 1px solid var(--line);
  }

  .slot {
    grid-area: 1 / 1;
    min-width: 0;
    margin: 0;
  }

  .slot.off {
    visibility: hidden;
  }

  .status {
    font-weight: 600;
  }

  .status:empty {
    display: none;
  }

  .picking {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: center;
    gap: 6px 12px;
  }

  .picked-count {
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .bar-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .bar-actions button {
    white-space: nowrap;
  }

  .failure {
    padding: 8px 10px 0;
  }

  .diff-body {
    --row: 24px;
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 0 0 8px;
    font-family: var(--font-log);
    font-size: var(--step--2);
    line-height: var(--row);
    outline: none;
    container-type: inline-size;
  }

  .diff-body:focus-visible {
    box-shadow: inset 0 0 0 2px var(--forest);
  }

  /* content-visibility clips each line to its own box, so we size the sheet to the longest
     line up front instead of letting the lines grow it. */
  .sheet {
    --no-width: calc(var(--digits) * 1ch + 14px);
    width: calc(2 * var(--no-width) + 3ch + var(--cols) * 1ch + 28px);
    min-width: 100%;
  }

  .line {
    display: flex;
    background: var(--field);
    content-visibility: auto;
    contain-intrinsic-size: auto var(--row);
  }

  .gutter {
    position: sticky;
    left: 0;
    z-index: 1;
    flex: none;
    display: flex;
    color: var(--muted);
    background: inherit;
    box-shadow: inset -1px 0 var(--line);
    user-select: none;
  }

  /* A swipe down the line numbers selects lines, so touch must not scroll there. */
  .gutter.pickable {
    cursor: pointer;
    touch-action: none;
  }

  .no {
    width: var(--no-width);
    padding-right: 6px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  .sign {
    width: 3ch;
    text-align: center;
  }

  .code {
    flex: 1;
    padding-right: 14px;
    white-space: pre;
    tab-size: 4;
  }

  /* Text you select to copy stays distinct from the ochre of lines selected for staging. */
  .code::selection,
  .code :global(*::selection) {
    background: color-mix(in srgb, var(--forest) 30%, transparent);
  }

  .code mark {
    color: inherit;
    border-radius: 2px;
  }

  .line.add mark {
    background: color-mix(in srgb, var(--forest) 24%, transparent);
  }

  .line.del mark {
    background: color-mix(in srgb, var(--fail) 22%, transparent);
  }

  .eol {
    margin-left: 1.5ch;
    padding: 0 4px;
    font-family: var(--font-ui);
    color: var(--muted);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-small);
  }

  .line.add {
    background: var(--ok-fill);
  }

  .line.del {
    background: var(--fail-fill);
  }

  .line.add .sign {
    color: var(--forest-hi);
  }

  .line.del .sign {
    color: var(--fail);
  }

  .line.meta {
    color: var(--muted);
  }

  .line.meta .code {
    padding-left: 14px;
  }

  .gutter.pickable:hover {
    color: var(--text);
    background: var(--highlight);
  }

  .line.picked .gutter {
    color: var(--text);
    background: var(--here);
    box-shadow:
      inset 3px 0 var(--ochre),
      inset -1px 0 var(--here-border);
  }

  .diff-body:focus-visible .line.cursor {
    outline: 2px solid var(--forest);
    outline-offset: -2px;
  }

  /* Block headers stick to the visible width, so their range and actions stay in view while
     you scroll a long line sideways. */
  .line.hunk {
    position: sticky;
    left: 0;
    z-index: 2;
    width: var(--view);
    align-items: center;
    gap: 10px;
    min-height: 32px;
    margin-top: 8px;
    padding-left: 14px;
    font-family: var(--font-ui);
    color: var(--muted);
    background: var(--surface);
    border-block: 1px solid var(--line);
  }

  .line.hunk:first-child {
    margin-top: 0;
    border-top: 0;
  }

  .line.hunk.pickable {
    cursor: pointer;
  }

  .line.hunk.pickable:hover {
    color: var(--text);
  }

  .line.hunk.picked {
    color: var(--text);
    background: var(--here);
    box-shadow: inset 3px 0 var(--ochre);
  }

  .range {
    flex: none;
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .context {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-log);
  }

  .hunk-actions {
    flex: none;
    display: flex;
    gap: 2px;
    margin-left: auto;
    padding-right: 6px;
  }

  .hunk-actions button {
    padding: 2px 8px;
    font-size: var(--step--1);
  }

  .danger-text:not(:disabled) {
    color: var(--fail);
  }

  @container (width < 420px) {
    .context,
    .wide {
      display: none;
    }

    .line.hunk {
      flex-wrap: wrap;
      row-gap: 0;
      padding-block: 2px;
    }

    .hunk-actions {
      margin-left: -8px;
    }
  }
</style>
