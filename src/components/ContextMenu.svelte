<script lang="ts" module>
  import type { MenuAction, MenuItem, Submenu } from '../lib/menu'

  // Callers build item lists from optional groups, so we drop the separators that end up
  // leading, trailing or doubled.
  function tidy(items: MenuItem[]) {
    const out: MenuItem[] = []
    for (const item of items) if (item !== 'separator' || (out.length && out.at(-1) !== 'separator')) out.push(item)
    if (out.at(-1) === 'separator') out.pop()
    return out
  }

  // When a submenu fits on neither side of its parent, we narrow it into the roomier side rather
  // than letting it cover the parent menu, and its long labels truncate.
  function fit(anchor: { left: number; right: number; top: number }, width: number, height: number) {
    const roomRight = innerWidth - 4 - anchor.right
    const roomLeft = anchor.left - 4
    const right = width <= roomRight || (width > roomLeft && roomRight >= roomLeft)
    const room = right ? roomRight : roomLeft
    const shown = Math.min(width, room)
    return {
      left: right ? anchor.right : anchor.left - shown,
      top: Math.max(4, Math.min(anchor.top, innerHeight - height - 4)),
      maxWidth: width > room ? room : null,
    }
  }
</script>

<script lang="ts">
  import { onMount, tick } from 'svelte'
  import { BACK_PRIORITY, useBack } from '../lib/back.svelte'

  let {
    x,
    y,
    label,
    items,
    scroller,
    onclose,
  }: {
    x: number
    y: number
    label: string
    items: MenuItem[]
    scroller?: EventTarget | null
    onclose: () => void
  } = $props()

  let menu: HTMLDivElement
  let submenu: HTMLDivElement | undefined = $state()
  let position = $state<{ left: number; top: number; maxWidth: number | null } | null>(null)
  let open = $state<{ index: number; left: number; top: number; maxWidth: number | null; placed: boolean } | null>(null)
  const returnFocus = document.activeElement as HTMLElement | null
  // Heading diagonally for the submenu crosses the items below its trigger, so we wait a
  // moment before closing it and cancel if the pointer reaches the submenu.
  let closeTimer: ReturnType<typeof setTimeout> | undefined

  let shown = $derived(tidy(items))
  let subItems = $derived.by(() => {
    const item = open ? shown[open.index] : null
    return item && item !== 'separator' && 'items' in item ? tidy(item.items) : []
  })

  function buttons(container: HTMLElement | undefined) {
    return Array.from(container?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not(:disabled)') ?? [])
  }

  // A live log or another pane scrolling shouldn't close a menu you opened elsewhere, so we only
  // watch the scrollers around the element you right-clicked.
  onMount(() => {
    const { width, height } = menu.getBoundingClientRect()
    position = fit({ left: x, right: x, top: y + height > innerHeight - 4 ? y - height : y }, width, height)
    // A hidden menu can't take focus, so we wait for the positioned one to render.
    tick().then(() => buttons(menu)[0]?.focus())
    const source = scroller ?? returnFocus
    const dismiss = (event: Event) => {
      const target = event.target
      if (!source || target === document || target === window || (target instanceof Node && source instanceof Node && target.contains(source))) close(false)
    }
    addEventListener('scroll', dismiss, true)
    return () => removeEventListener('scroll', dismiss, true)
  })

  function close(refocus = true) {
    clearTimeout(closeTimer)
    onclose()
    if (refocus) returnFocus?.focus()
  }

  function choose(item: MenuAction) {
    close()
    item.run()
  }

  async function openSubmenu(index: number, button: HTMLElement, focus: boolean) {
    clearTimeout(closeTimer)
    if (open?.index === index) {
      if (focus) buttons(submenu)[0]?.focus()
      return
    }
    const rect = button.getBoundingClientRect()
    open = { index, left: rect.right, top: rect.top - 5, maxWidth: null, placed: false }
    await tick()
    if (!submenu || open?.index !== index) return
    const { width, height } = submenu.getBoundingClientRect()
    open = { index, ...fit({ left: rect.left, right: rect.right, top: rect.top - 5 }, width, height), placed: true }
    await tick()
    if (focus) buttons(submenu)[0]?.focus()
  }

  function closeSubmenu() {
    const index = open?.index
    open = null
    if (index !== undefined) menu.querySelector<HTMLElement>(`[data-index="${index}"]`)?.focus()
  }

  function onKey(event: KeyboardEvent) {
    const inSubmenu = !!submenu?.contains(document.activeElement)
    const all = buttons(inSubmenu ? submenu : menu)
    const current = document.activeElement as HTMLButtonElement
    const index = all.indexOf(current)
    if (event.key === 'ArrowDown') all[(index + 1) % all.length]?.focus()
    else if (event.key === 'ArrowUp') all[(index - 1 + all.length) % all.length]?.focus()
    else if (event.key === 'Home') all[0]?.focus()
    else if (event.key === 'End') all[all.length - 1]?.focus()
    else if (event.key === 'ArrowRight' && !inSubmenu && current.dataset.submenu) openSubmenu(Number(current.dataset.index), current, true)
    else if ((event.key === 'ArrowLeft' || event.key === 'Escape') && inSubmenu) closeSubmenu()
    else if (event.key === 'Escape') close()
    else if (event.key === 'Tab') close(false)
    else return
    event.preventDefault()
    event.stopPropagation()
  }

  function outside(target: Node) {
    return !menu.contains(target) && !submenu?.contains(target)
  }

  useBack(BACK_PRIORITY.overlay, () => {
    if (open) closeSubmenu()
    else close()
    return true
  })
</script>

<svelte:window onpointerdown={(e) => e.button !== 3 && e.button !== 4 && outside(e.target as Node) && close(false)} onblur={() => close(false)} onresize={() => close(false)} />

{#snippet entries(list: MenuItem[], nested: boolean)}
  {@const narrowed = !!(nested ? open?.maxWidth : position?.maxWidth)}
  {#each list as item, i (i)}
    {#if item === 'separator'}
      <div class="separator" role="separator"></div>
    {:else if 'items' in item}
      <button
        role="menuitem"
        tabindex="-1"
        aria-haspopup="menu"
        aria-expanded={open?.index === i}
        data-index={i}
        data-submenu="true"
        class:expanded={open?.index === i}
        title={narrowed ? item.label : undefined}
        onclick={(e) => openSubmenu(i, e.currentTarget, true)}
        onpointerenter={(e) => openSubmenu(i, e.currentTarget, false)}
      >
        <span>{item.label}</span>
        <svg viewBox="0 0 12 12" aria-hidden="true"><path d="M4.5 3l3 3-3 3" /></svg>
      </button>
    {:else}
      <button
        role="menuitem"
        tabindex="-1"
        data-index={i}
        class:danger={item.danger}
        title={narrowed ? item.label : undefined}
        disabled={item.disabled}
        onclick={() => choose(item)}
        onpointerenter={() => !nested && open && (closeTimer = setTimeout(() => (open = null), 250))}
      >
        <span>{item.label}</span>
        {#if item.shortcut}<kbd>{item.shortcut}</kbd>{/if}
      </button>
    {/if}
  {/each}
{/snippet}

<div
  class="menu"
  role="menu"
  aria-label={label}
  tabindex="-1"
  bind:this={menu}
  style:left="{position?.left ?? x}px"
  style:top="{position?.top ?? y}px"
  style:max-width={position?.maxWidth ? `${position.maxWidth}px` : null}
  style:visibility={position ? 'visible' : 'hidden'}
  onkeydown={onKey}
  oncontextmenu={(e) => e.preventDefault()}
>
  {@render entries(shown, false)}
</div>

{#if open && subItems.length}
  <div
    class="menu"
    role="menu"
    aria-label={(shown[open.index] as Submenu).label}
    tabindex="-1"
    bind:this={submenu}
    onpointerenter={() => clearTimeout(closeTimer)}
    style:left="{open.left}px"
    style:top="{open.top}px"
    style:max-width={open.maxWidth ? `${open.maxWidth}px` : null}
    style:visibility={open.placed ? 'visible' : 'hidden'}
    onkeydown={onKey}
    oncontextmenu={(e) => e.preventDefault()}
  >
    {@render entries(subItems, true)}
  </div>
{/if}

<style>
  .menu {
    position: fixed;
    z-index: 20;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    min-width: 200px;
    padding: 4px;
    font-size: var(--step--1);
    background: var(--line);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-panel);
    box-shadow: 0 12px 32px var(--shadow);
  }

  button {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 24px;
    padding: 4px 8px;
    text-align: left;
    font-weight: 400;
    background: transparent;
    border-color: transparent;
  }

  button:hover:not(:disabled),
  button:focus-visible,
  button.expanded {
    background: var(--menu-hover);
  }

  button:focus-visible {
    outline: 2px solid var(--forest);
    outline-offset: -2px;
  }

  button:disabled {
    border-color: transparent;
  }

  button.danger:not(:disabled) {
    color: var(--fail);
  }

  /* In a narrowed menu the shortcut column gives up its room well before the label does. */
  button span,
  kbd {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  kbd {
    flex: 0 8 auto;
    border: 0;
    padding: 0;
  }

  svg {
    flex: none;
    width: 12px;
    height: 12px;
    fill: none;
    stroke: var(--muted);
    stroke-width: 1.6;
  }

  .separator {
    height: 1px;
    margin: 4px 6px;
    background: var(--line-strong);
  }
</style>
