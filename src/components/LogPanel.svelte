<script lang="ts">
  import { tick, untrack } from 'svelte'
  import { errorText } from '../lib/api'
  import type { Head, Level, LogModel } from '../lib/logs'
  import { BACK_PRIORITY, useBack } from '../lib/back.svelte'
  import { paneFocus } from '../lib/panes.svelte'
  import { isMac, shortcut } from '../lib/platform'
  import ContextMenu from './ContextMenu.svelte'
  import type { MenuItem } from '../lib/menu'

  // The model is plain data for speed, so its owner bumps `version` when lines arrive and
  // `generation` when a new log replaces the old one.
  let {
    model,
    version,
    generation,
    live = false,
    framed = false,
    loading = false,
    label,
    empty = 'Nothing logged yet.',
  }: {
    model: LogModel
    version: number
    generation: number
    live?: boolean
    framed?: boolean
    loading?: boolean
    label: string
    empty?: string
  } = $props()

  // Rows have a fixed height, so we can render only the ones on screen. A modded debug.log
  // has tens of thousands of lines.
  const ROW = 20
  const OVERSCAN = 30
  const MAX_WIDTH_CH = 2000
  const FILTERS: { level: Level; label: string }[] = [
    { level: 'error', label: 'Errors' },
    { level: 'warn', label: 'Warnings' },
    { level: 'info', label: 'Info' },
    { level: 'debug', label: 'Debug' },
  ]
  const CONTEXT_CHOICES = [0, 1, 2, 3, 5, 10, 25]
  const CONTEXT_KEY = 'packraven.logContext'
  const BACK_KEYS = isMac ? 'Option+Left' : 'Alt+Left'

  const collapsed = new Set<number>()
  let foldVersion = $state(0)
  let note = $state<string | null>(null)
  let shown = $state<Record<Level, boolean>>({ error: true, warn: true, info: true, debug: true, none: true })
  let query = $state('')
  let leftQuery = $state('')
  let context = $state(storedContext())
  let scrollTop = $state(0)
  let viewportHeight = $state(0)
  let pinned = $state(true)
  let selected = $state<number | null>(null)
  let spoken = $state('')
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null)
  let viewport: HTMLDivElement = $state()!
  let filterField: HTMLInputElement
  let dragging = false
  let lastInput = 0
  let programmaticTop = -1

  // A new log starts at its end, with nothing folded or selected.
  $effect(() => {
    void generation
    untrack(() => {
      collapsed.clear()
      foldVersion++
      selected = null
      pinned = true
    })
  })

  // Filtering scans every line, so we wait for a pause in typing before applying it.
  let needle = $state('')
  $effect(() => {
    const next = query.trim().toLowerCase()
    const timer = setTimeout(() => (needle = next), next ? 150 : 0)
    return () => clearTimeout(timer)
  })

  // Lines only ever get appended, and clear() swaps in a new array, so we lowercase each line once.
  let lowered: string[] = []
  let loweredFrom: string[] | null = null

  function lowerLines() {
    if (loweredFrom !== model.lines) {
      loweredFrom = model.lines
      lowered = []
    }
    for (let i = lowered.length; i < model.lines.length; i++) lowered.push(model.lines[i].toLowerCase())
    return lowered
  }

  // A filter keeps whole entries, so a matching error keeps its stack trace.
  let matching = $derived.by(() => {
    void version
    void generation
    if (!needle) return null
    const lines = lowerLines()
    const entries = new Uint8Array(model.entries.length)
    for (let i = 0; i < lines.length; i++) {
      if (lines[i].includes(needle)) entries[model.lineEntry[i]] = 1
    }
    return entries
  })

  // Like grep -C, context counts the lines the level filters and folds leave showing, and a line
  // that starts a new run after skipped ones goes in `breaks` so we can mark the gap.
  let view = $derived.by(() => {
    void version
    void generation
    void foldVersion
    const visible: number[] = []
    for (let i = 0; i < model.lines.length; i++) {
      const entry = model.lineEntry[i]
      if (!shown[model.entries[entry].level]) continue
      if (i !== model.entries[entry].start && collapsed.has(entry)) continue
      visible.push(i)
    }
    const breaks = new Set<number>()
    if (!matching) return { rows: visible, breaks }
    const hit = (k: number) => matching[model.lineEntry[visible[k]]] === 1
    const near = new Uint8Array(visible.length)
    for (let k = 0, after = 0; k < visible.length; k++) {
      if (hit(k)) {
        near[k] = 1
        after = context
      } else if (after > 0) {
        near[k] = 1
        after--
      }
    }
    for (let k = visible.length - 1, before = 0; k >= 0; k--) {
      if (hit(k)) {
        before = context
      } else if (before > 0) {
        near[k] = 1
        before--
      }
    }
    const rows: number[] = []
    for (let k = 0; k < visible.length; k++) {
      if (!near[k]) continue
      if (context && rows.length && !near[k - 1]) breaks.add(visible[k])
      rows.push(visible[k])
    }
    return { rows, breaks }
  })
  let rows = $derived(view.rows)

  let first = $derived(Math.max(0, Math.floor(scrollTop / ROW) - OVERSCAN))
  let last = $derived(Math.min(rows.length, Math.ceil((scrollTop + viewportHeight) / ROW) + OVERSCAN))
  let width = $derived.by(() => {
    void version
    void generation
    return Math.min(model.longest + 12, MAX_WIDTH_CH)
  })
  let counts = $derived.by(() => {
    void version
    void generation
    return { ...model.counts }
  })
  let matchCount = $derived(matching ? matching.reduce((sum, hit) => sum + hit, 0) : 0)
  let selectedShown = $derived(selected !== null && rows.includes(selected))
  let levelsHidden = $derived(FILTERS.some(({ level }) => !shown[level] && counts[level]))

  // Scroll events arrive a frame later, so when we scroll we record the position ourselves and
  // render the right rows straight away.
  function scrollTo(top: number) {
    if (!viewport) return
    viewport.scrollTop = top
    scrollTop = viewport.scrollTop
    programmaticTop = scrollTop
  }

  function scrollToEnd() {
    if (viewport) scrollTo(viewport.scrollHeight)
  }

  // Following stops only when you scroll up yourself. Content arriving or the pane resizing
  // also fire scroll events, and those shouldn't let go of the end.
  function onScroll() {
    // A scroll event queued before the panel unmounts still arrives after.
    if (!viewport) return
    scrollTop = viewport.scrollTop
    if (scrollTop === programmaticTop) {
      programmaticTop = -1
      return
    }
    const nearBottom = viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight < ROW * 2
    if (nearBottom) pinned = true
    else if (dragging || performance.now() - lastInput < 300) pinned = false
  }

  function userInput() {
    lastInput = performance.now()
  }

  $effect(() => {
    void version
    void generation
    untrack(async () => {
      if (!pinned) return
      await tick()
      scrollToEnd()
    })
  })

  $effect(() => {
    const observer = new ResizeObserver(() => pinned && scrollToEnd())
    observer.observe(viewport)
    return () => observer.disconnect()
  })

  // When a filter changes, we keep the selected line in view, or the end if you were following.
  $effect(() => {
    void shown.error
    void shown.warn
    void shown.info
    void shown.debug
    void needle
    void context
    void foldVersion
    untrack(async () => {
      await tick()
      const at = selected === null ? -1 : rows.indexOf(selected)
      if (at >= 0) reveal(at)
      else if (pinned) scrollToEnd()
    })
  })

  function reveal(rowIndex: number) {
    if (!viewport) return
    const top = rowIndex * ROW
    if (top < viewport.scrollTop || top + ROW > viewport.scrollTop + viewport.clientHeight) {
      scrollTo(Math.max(0, top - viewport.clientHeight / 3))
    }
    if (needle) revealMark()
  }

  // A match can sit past the right edge of a long line, so we scroll sideways to it.
  async function revealMark() {
    await tick()
    const mark = viewport?.querySelector<HTMLElement>('.row.selected mark')
    if (!mark) return
    const box = viewport.getBoundingClientRect()
    const at = mark.getBoundingClientRect()
    if (at.left < box.left || at.right > box.right) viewport.scrollLeft += at.left - box.left - box.width / 3
  }

  // Picking a line in a live log stops following, so new lines don't scroll it away.
  function select(line: number) {
    selected = line
    if (live) pinned = false
  }

  function toggleLevel(level: Level) {
    shown[level] = !shown[level]
  }

  function folded(entry: number) {
    void foldVersion
    return collapsed.has(entry)
  }

  function toggleFold(entry: number) {
    if (collapsed.has(entry)) collapsed.delete(entry)
    else collapsed.add(entry)
    foldVersion++
  }

  function isStart(line: number) {
    return model.entries[model.lineEntry[line]].start === line
  }

  // Like IDEs, F8 and Shift+F8 step through errors, starting from the selected line, or else
  // from the edge of the view they move away from, so errors already on screen count.
  function stepError(direction: 1 | -1) {
    const edge = direction > 0 ? Math.floor(viewport.scrollTop / ROW) - 1 : Math.ceil((viewport.scrollTop + viewport.clientHeight) / ROW)
    const from = selected !== null && rows.includes(selected) ? rows.indexOf(selected) : edge
    for (let at = from + direction; at >= 0 && at < rows.length; at += direction) {
      const line = rows[at]
      if (isStart(line) && model.entries[model.lineEntry[line]].level === 'error') {
        selected = line
        pinned = false
        reveal(at)
        return
      }
    }
  }

  function isHit(line: number) {
    return matching !== null && matching[model.lineEntry[line]] === 1
  }

  // We remember the filter you leave, so you can go back to the other matches.
  function leaveFilter() {
    if (!query) return
    leftQuery = query
    query = ''
    spoken = `Filter cleared. Press ${BACK_KEYS} to go back to “${leftQuery}”.`
  }

  function setQuery(text: string) {
    query = text
    leftQuery = ''
  }

  function showInFullLog(line: number) {
    shown[model.entries[model.lineEntry[line]].level] = true
    leaveFilter()
    selected = line
    pinned = false
    spoken = `Showing line ${line + 1} in the full log. Press ${BACK_KEYS} to go back to “${leftQuery}”.`
  }

  // We skip the typing pause so the matches are there to select straight away.
  async function applyFilterNow() {
    needle = query.trim().toLowerCase()
    await tick()
  }

  // If the line you moved to in the full log is filtered out, we select the nearest line above it.
  async function backToFilter() {
    if (!leftQuery) return
    setQuery(leftQuery)
    await applyFilterNow()
    viewport.focus({ preventScroll: true })
    if (selected === null || !rows.length || rows.includes(selected)) return
    const at = Math.max(0, rows.findLastIndex((line) => line < selected!))
    select(rows[at])
    reveal(at)
  }

  // Enter in the filter field moves to the results, starting at the first match.
  async function enterResults() {
    await applyFilterNow()
    if (!rows.length) return
    const current = selected !== null ? rows.indexOf(selected) : -1
    const at = current >= 0 ? current : Math.max(0, rows.findIndex(isHit))
    select(rows[at])
    pinned = false
    viewport.focus({ preventScroll: true })
    reveal(at)
    announce(rows[at])
  }

  function onFilterKey(event: KeyboardEvent) {
    if (event.key === 'Enter') {
      event.preventDefault()
      enterResults()
    } else if (event.key === 'Escape' && query) {
      event.preventDefault()
      leaveFilter()
    }
  }

  function showAllLevels() {
    for (const { level } of FILTERS) shown[level] = true
  }

  // Matches in the thread or logger columns have nothing to highlight in the message, and a narrow
  // pane hides those columns, so we name the column that matched.
  function matchedColumn(head: Head) {
    if (!needle || head.message.toLowerCase().includes(needle)) return null
    if (`${head.logger}/${head.marker}`.toLowerCase().includes(needle)) return { column: 'logger', shownWide: shortLogger(head.logger).toLowerCase().includes(needle) }
    if (head.thread.toLowerCase().includes(needle)) return { column: 'thread', shownWide: true }
    return null
  }

  function storedContext() {
    try {
      const stored = Number(localStorage.getItem(CONTEXT_KEY))
      return CONTEXT_CHOICES.includes(stored) ? stored : 0
    } catch {
      return 0
    }
  }

  function setContext(lines: number) {
    context = lines
    try {
      localStorage.setItem(CONTEXT_KEY, String(lines))
    } catch {}
  }

  function jumpToLatest() {
    pinned = true
    selected = null
    scrollToEnd()
  }

  // You can move the selected line with the arrow keys and open its actions with the menu key or
  // Shift+F10, so copying and filtering work without a mouse.
  function moveSelection(by: number) {
    if (!rows.length) return
    const at = selected !== null ? rows.indexOf(selected) : -1
    const from = at >= 0 ? at : Math.floor(viewport.scrollTop / ROW) - (by > 0 ? 1 : 0)
    const next = Math.max(0, Math.min(rows.length - 1, from + by))
    select(rows[next])
    pinned = false
    reveal(next)
    announce(rows[next])
  }

  function announce(line: number) {
    const kind = !matching || !context ? '' : `${isHit(line) ? 'Match' : 'Context'}${view.breaks.has(line) ? ' after skipped lines' : ''}, `
    spoken = `${kind}Line ${line + 1}: ${model.lines[line]}`
  }

  function onKey(event: KeyboardEvent) {
    userInput()
    const command = event.ctrlKey || event.metaKey
    const page = Math.max(1, Math.floor(viewport.clientHeight / ROW) - 1)
    if (event.key === 'F8') {
      event.preventDefault()
      stepError(event.shiftKey ? -1 : 1)
      if (selected !== null) announce(selected)
    } else if (command && event.key.toLowerCase() === 'f') {
      event.preventDefault()
      filterField.focus()
      filterField.select()
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault()
      moveSelection(event.key === 'ArrowDown' ? 1 : -1)
    } else if (event.key === 'PageDown' || event.key === 'PageUp') {
      event.preventDefault()
      moveSelection(event.key === 'PageDown' ? page : -page)
    } else if (selected !== null && (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10'))) {
      event.preventDefault()
      const row = viewport.querySelector<HTMLElement>('.row.selected')?.getBoundingClientRect()
      const box = viewport.getBoundingClientRect()
      openMenu(row ? row.left + 80 : box.left + 40, row ? row.bottom : box.top + 40, selected)
    } else if (selected !== null && needle && event.key === 'Enter') {
      event.preventDefault()
      showInFullLog(selected)
    } else if (selected !== null && command && event.key.toLowerCase() === 'c' && window.getSelection()?.isCollapsed !== false) {
      event.preventDefault()
      copy(model.lines[selected])
    } else if (event.key === 'Escape' && query) {
      event.preventDefault()
      leaveFilter()
    } else if (event.altKey && event.key === 'ArrowLeft' && !query && leftQuery) {
      event.preventDefault()
      backToFilter()
    }
  }

  const inFocusedPane = paneFocus()
  let panelEl = $state<HTMLDivElement>()

  // When two logs share a pane (TestRun), clear the focused log's filter before its sibling's.
  useBack(BACK_PRIORITY.filter + 1, () => {
    if (!inFocusedPane() || !query || !viewport?.offsetParent) return false
    if (!panelEl?.contains(document.activeElement)) return false
    leaveFilter()
    return true
  })

  useBack(BACK_PRIORITY.filter, () => {
    if (!inFocusedPane() || !query || !viewport?.offsetParent) return false
    leaveFilter()
    return true
  })

  useBack(BACK_PRIORITY.mode, (source) => {
    if (source !== 'back' || !inFocusedPane() || query || !leftQuery || !viewport?.offsetParent) return false
    if (!panelEl?.contains(document.activeElement)) return false
    backToFilter()
    return true
  })

  function entryText(entry: number) {
    return model.lines.slice(model.entries[entry].start, model.entryEnd(entry)).join('\n')
  }

  function copy(text: string) {
    navigator.clipboard.writeText(text).catch((e) => (note = `Couldn't copy: ${errorText(e)}`))
  }

  function onRowMenu(event: MouseEvent, line: number) {
    event.preventDefault()
    event.stopPropagation()
    openMenu(event.clientX, event.clientY, line)
  }

  function openMenu(x: number, y: number, line: number) {
    select(line)
    const entry = model.lineEntry[line]
    const logger = model.entries[entry].head?.logger
    const lines = model.entryEnd(entry) - model.entries[entry].start
    menu = {
      x,
      y,
      items: [
        ...(needle ? [{ label: 'Show in full log', shortcut: 'Enter', run: () => showInFullLog(line) }, 'separator' as const] : []),
        { label: 'Copy line', shortcut: shortcut('Ctrl+C'), run: () => copy(model.lines[line]) },
        ...(lines > 1 ? [{ label: `Copy entry (${lines} lines)`, run: () => copy(entryText(entry)) }] : []),
        'separator',
        ...(logger ? [{ label: `Filter to ${shortLogger(logger)}`, run: () => setQuery(logger) }] : []),
      ],
    }
  }

  function shortLogger(logger: string) {
    return logger.slice(logger.lastIndexOf('.') + 1)
  }

  function parts(text: string) {
    if (!needle) return [{ text, hit: false }]
    const out: { text: string; hit: boolean }[] = []
    const lower = text.toLowerCase()
    let at = 0
    for (let found = lower.indexOf(needle); found >= 0; found = lower.indexOf(needle, found + needle.length)) {
      if (found > at) out.push({ text: text.slice(at, found), hit: false })
      out.push({ text: text.slice(found, found + needle.length), hit: true })
      at = found + needle.length
    }
    if (at < text.length) out.push({ text: text.slice(at), hit: false })
    return out
  }
</script>

<svelte:window onpointerup={() => (dragging = false)} />

{#snippet marked(text: string)}
  {#each parts(text) as part, i (i)}{#if part.hit}<mark>{part.text}</mark>{:else}{part.text}{/if}{/each}
{/snippet}

<div class="panel" class:framed bind:this={panelEl}>
  <div class="bar">
    <span class="levels" role="group" aria-label="Show levels">
      {#each FILTERS as { level, label: filterLabel } (level)}
        <span class="level-group">
          <button class="filter {level}" aria-pressed={shown[level]} disabled={!counts[level] && !loading} onclick={() => toggleLevel(level)}>
            {filterLabel} <span class="count">{counts[level].toLocaleString()}</span>
          </button>
          {#if level === 'error'}
            <button class="ghost" aria-label="Previous error" title="Previous error (Shift+F8)" disabled={!counts.error} onclick={() => stepError(-1)}>
              <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M3 7.5l3-3 3 3" /></svg>
            </button>
            <button class="ghost" aria-label="Next error" title="Next error (F8)" disabled={!counts.error} onclick={() => stepError(1)}>
              <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M3 4.5l3 3 3-3" /></svg>
            </button>
          {/if}
        </span>
      {/each}
    </span>
    <span class="search">
      <input
        bind:this={filterField}
        bind:value={query}
        oninput={() => (leftQuery = '')}
        onkeydown={onFilterKey}
        type="search"
        placeholder="Filter lines"
        aria-label="Filter {label.toLowerCase()}"
        title="Filter lines ({shortcut('Ctrl+F')}). Press Enter to go to the matches."
        spellcheck="false"
      />
      {#if needle}
        <select class="context" value={context} onchange={(e) => setContext(Number(e.currentTarget.value))} aria-label="Lines to show around each match" title="Lines to show around each match">
          {#each CONTEXT_CHOICES as lines (lines)}<option value={lines}>{lines ? `± ${lines} ${lines === 1 ? 'line' : 'lines'}` : 'Matches only'}</option>{/each}
        </select>
      {/if}
      <span class="muted small" aria-live="polite">{needle ? `${matchCount.toLocaleString()} ${matchCount === 1 ? 'entry' : 'entries'}` : ''}</span>
      {#if needle && selectedShown}
        <button class="link small" title="Show this line among all lines (Enter or double-click)" onclick={() => selected !== null && showInFullLog(selected)}>Show in full log</button>
      {:else if !query && leftQuery}
        <button class="link small back" title="Back to “{leftQuery}” ({BACK_KEYS})" aria-label="Back to filter “{leftQuery}”" onclick={backToFilter}>Back to “{leftQuery}”</button>
      {/if}
    </span>
  </div>
  {#if note}<p class="note failed small">{note}</p>{/if}

  <div class="frame">
    <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions (the log takes arrows, F8, Ctrl+F and the menu key) -->
    <div
      class="log"
      bind:this={viewport}
      bind:clientHeight={viewportHeight}
      onscroll={onScroll}
      onwheel={userInput}
      onkeydown={onKey}
      onpointerdown={() => (dragging = true)}
      role="log"
      aria-live="off"
      aria-label={label}
      tabindex="0"
    >
      <div class="content" style:height="{rows.length * ROW}px" style:width="calc({width}ch + 80px)">
        {#each rows.slice(first, last) as line, k (`${generation}:${line}`)}
          {@const entryIndex = model.lineEntry[line]}
          {@const entry = model.entries[entryIndex]}
          {@const start = entry.start === line}
          {@const extra = start ? model.entryEnd(entryIndex) - line - 1 : 0}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions (the log container moves the selection with arrow keys) -->
          <div
            class="row {entry.level}"
            class:app={entry.stream === 'info'}
            class:start
            class:selected={selected === line}
            class:context={matching && !matching[entryIndex]}
            class:hit={matching && context && matching[entryIndex]}
            class:gap={view.breaks.has(line)}
            style:top="{(first + k) * ROW}px"
            onclick={() => select(line)}
            ondblclick={() => needle && (window.getSelection()?.removeAllRanges(), showInFullLog(line))}
            oncontextmenu={(e) => onRowMenu(e, line)}
          >
            <span class="number">{line + 1}</span>
            {#if extra}
              <button class="fold" aria-label="{folded(entryIndex) ? 'Show' : 'Hide'} {extra} more lines" onclick={(e) => (e.stopPropagation(), toggleFold(entryIndex))}>
                <svg viewBox="0 0 12 12" class:open={!folded(entryIndex)} aria-hidden="true"><path d="M4.5 3l3 3-3 3" /></svg>
              </button>
            {:else}
              <span class="fold" aria-hidden="true"></span>
            {/if}
            {#if start && entry.head}
              <span class="time">{entry.head.time}</span>
              <span class="level">{entry.head.level}</span>
              {@const found = matchedColumn(entry.head)}
              <span class="thread" title={entry.head.thread}>{@render marked(entry.head.thread)}</span>
              {#if entry.head.logger}<span class="logger" title={[entry.head.logger, entry.head.marker].filter(Boolean).join(' / ')}>{@render marked(shortLogger(entry.head.logger))}</span>{/if}
              {#if found}<span class="found-in" class:shown-wide={found.shownWide} title={found.column === 'logger' ? [entry.head.logger, entry.head.marker].filter(Boolean).join(' / ') : entry.head.thread}>in {found.column}</span>{/if}
              <span class="message">{@render marked(entry.head.message)}</span>
              {#if extra && folded(entryIndex)}<span class="hidden-lines">{extra} more {extra === 1 ? 'line' : 'lines'}</span>{/if}
            {:else}
              <span class="message">{@render marked(model.lines[line])}</span>
            {/if}
          </div>
        {/each}
      </div>
      {#if loading}
        <p class="empty muted">Reading the log…</p>
      {:else if !rows.length}
        <p class="empty muted">
          {#if !model.lines.length}
            {empty}
          {:else if needle}
            No lines match “{query.trim()}”{levelsHidden ? ' at the levels shown' : ''}.
            <button class="link" onclick={() => setQuery('')}>Clear filter</button>
            {#if levelsHidden}<button class="link" onclick={showAllLevels}>Show all levels</button>{/if}
          {:else}
            No lines at the levels shown. <button class="link" onclick={showAllLevels}>Show all levels</button>
          {/if}
        </p>
      {/if}
    </div>
    {#if live && !pinned}
      <button class="jump" onclick={jumpToLatest}>Jump to latest</button>
    {/if}
    <span class="visually-hidden" aria-live="polite">{spoken}</span>
  </div>
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} label="Log line actions" items={menu.items} scroller={viewport} onclose={() => (menu = null)} />
{/if}

<style>
  .panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    container-type: inline-size;
  }

  .panel.framed {
    min-height: 240px;
    overflow: hidden;
    background: var(--field);
    border: 1px solid var(--line);
    border-radius: var(--radius-panel);
  }

  .bar {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: center;
    gap: 6px 12px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--line);
  }

  .levels,
  .search,
  .level-group {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .levels {
    flex-wrap: wrap;
  }

  .level-group {
    gap: 2px;
  }

  .filter {
    padding: 2px 8px;
    font-weight: 400;
    font-size: var(--step--1);
    white-space: nowrap;
    color: var(--muted);
    background: transparent;
    border-color: var(--line);
    border-radius: 999px;
  }

  .filter[aria-pressed='true']:not(:disabled) {
    color: var(--text);
    background: var(--surface);
    border-color: var(--line-strong);
  }

  .filter.error[aria-pressed='true']:not(:disabled) .count {
    color: var(--fail);
  }

  .filter.warn[aria-pressed='true']:not(:disabled) .count {
    color: var(--warn);
  }

  .count {
    font-weight: 700;
    font-variant-numeric: tabular-nums;
  }

  .ghost {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    padding: 0;
  }

  .ghost svg {
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.6;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .back {
    max-width: 22ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .search input,
  .search select {
    padding: 3px 8px;
    font-size: var(--step--1);
  }

  .search input {
    width: 220px;
    max-width: 100%;
  }

  .small {
    font-size: var(--step--1);
  }

  .note {
    padding: 6px 12px;
    border-bottom: 1px solid var(--line);
  }

  .frame {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
  }

  .log {
    position: relative;
    flex: 1;
    min-width: 0;
    overflow: auto;
    overflow-anchor: none;
    font-family: var(--font-log);
    font-size: var(--step--2);
    outline: none;
  }

  .log:focus-visible {
    box-shadow: inset 0 0 0 2px var(--forest);
  }

  .content {
    position: relative;
    min-width: 100%;
  }

  .row {
    position: absolute;
    left: 0;
    right: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    height: 20px;
    padding-right: 12px;
    white-space: pre;
    tab-size: 4;
    color: var(--text);
  }

  .row:hover {
    background: var(--surface);
  }

  .row.selected {
    background: var(--here);
  }

  .row.error {
    color: var(--fail);
  }

  .row.error.start:not(.selected) {
    background: var(--fail-fill);
  }

  .row.warn .message {
    color: var(--warn);
  }

  .row.debug {
    color: var(--muted);
  }

  /* Context lines recede so the matches stand out, but an error or warning nearby keeps its
     level's color, since it's often what you were looking for. */
  .row.context,
  .row.context .message {
    color: var(--muted);
    font-weight: 400;
  }

  .row.context.error .level {
    color: var(--fail);
  }

  .row.context.warn .level {
    color: var(--warn);
  }

  .row.context.error.start:not(.selected) {
    background: transparent;
  }

  .row.hit .number {
    color: var(--text);
    background: var(--hit-soft);
  }

  .row.gap {
    border-top: 1px dashed var(--muted);
  }

  /* packraven's own messages, like "Building the server pack", stand apart from the game's. */
  .row.app .message {
    color: var(--forest);
    font-weight: 700;
  }

  .number {
    flex: none;
    width: 6ch;
    padding-right: 4px;
    text-align: right;
    color: var(--muted);
    background: var(--paper);
    border-right: 1px solid var(--line);
    user-select: none;
    font-variant-numeric: tabular-nums;
  }

  .fold {
    flex: none;
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    padding: 0;
    background: transparent;
    border: 0;
  }

  .fold svg {
    width: 11px;
    height: 11px;
    fill: none;
    stroke: var(--muted);
    stroke-width: 1.6;
    transition: transform 0.1s ease;
  }

  .fold svg.open {
    transform: rotate(90deg);
  }

  .time {
    flex: none;
    min-width: 8ch;
    color: var(--muted);
  }

  .level {
    flex: none;
    width: 5ch;
    font-weight: 700;
  }

  /* Fixed columns keep the messages lined up; the full names show on hover. */
  .thread,
  .logger {
    flex: none;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--muted);
  }

  .thread {
    width: 13ch;
  }

  .logger {
    width: 16ch;
    color: var(--forest);
  }

  /* A narrow pane, like the Run view's side-by-side logs, keeps its width for the message. */
  @container (max-width: 760px) {
    .thread,
    .logger {
      display: none;
    }

    .search input {
      width: 140px;
    }
  }

  @container (min-width: 761px) {
    .found-in.shown-wide {
      display: none;
    }
  }

  .message {
    flex: none;
  }

  .found-in {
    flex: none;
    padding: 0 6px;
    font-family: var(--font-ui);
    color: var(--text);
    background: var(--hit-soft);
    border: 1px solid var(--hit-edge);
    border-radius: 999px;
  }

  .hidden-lines {
    flex: none;
    padding: 0 6px;
    font-family: var(--font-ui);
    color: var(--muted);
    border: 1px solid var(--line);
    border-radius: 999px;
  }

  mark {
    color: var(--hit-ink);
    background: var(--hit);
    border-radius: 2px;
  }

  .empty {
    position: absolute;
    top: 16px;
    left: 0;
    right: 0;
    text-align: center;
    font-family: var(--font-ui);
  }

  .empty .link {
    margin-left: 8px;
  }

  .jump {
    position: absolute;
    right: 18px;
    bottom: 14px;
    padding: 5px 12px;
    font-size: var(--step--1);
    box-shadow: 0 6px 18px var(--shadow);
  }

  @media (prefers-reduced-motion: reduce) {
    .fold svg {
      transition: none;
    }
  }
</style>
