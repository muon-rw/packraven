import { clamp } from './storedSplit.svelte'

export interface SplitterConfig {
  /** `vertical` divides side-by-side panes; `horizontal` divides stacked ones (ARIA's naming). */
  orientation: 'vertical' | 'horizontal'
  /** The pane you resize sits after the handle (below or to the right), so dragging toward it shrinks it. */
  after?: boolean
  label: string
  controls?: string
  value: () => number
  min: () => number
  max: () => number
  onresize: (value: number) => void
  onreset: () => void
}

const STEP = 16
const BIG_STEP = 64

/**
 * Turns `node` into a window splitter: drag it, use the arrow keys (Shift for bigger steps), Home or
 * End, or double-click or press Enter to reset. Works as a Svelte action and on plain DOM, like the
 * handle we add to CodeMirror's problems panel.
 */
export function splitter(node: HTMLElement, initial: SplitterConfig) {
  let config = initial
  let drag: { at: number; value: number } | null = null

  const sign = () => (config.after ? -1 : 1)
  const pointer = (event: PointerEvent) => (config.orientation === 'vertical' ? event.clientX : event.clientY)
  const resize = (value: number) => config.onresize(clamp(value, config.min(), config.max()))

  function sync() {
    const min = config.min()
    const span = Math.max(config.max() - min, 1)
    node.classList.add('splitter')
    node.setAttribute('role', 'separator')
    node.setAttribute('tabindex', '0')
    node.setAttribute('aria-orientation', config.orientation)
    node.setAttribute('aria-label', config.label)
    if (config.controls) node.setAttribute('aria-controls', config.controls)
    node.setAttribute('aria-valuemin', '0')
    node.setAttribute('aria-valuemax', '100')
    node.setAttribute('aria-valuenow', String(Math.round((100 * (config.value() - min)) / span)))
    node.title = 'Drag to resize. Double-click to reset.'
  }

  function start(event: PointerEvent) {
    if (event.button !== 0) return
    // Keeps the drag from selecting text in the panes.
    event.preventDefault()
    node.setPointerCapture(event.pointerId)
    node.classList.add('dragging')
    drag = { at: pointer(event), value: config.value() }
  }

  function move(event: PointerEvent) {
    if (drag) resize(drag.value + sign() * (pointer(event) - drag.at))
  }

  function end() {
    drag = null
    node.classList.remove('dragging')
  }

  function reset() {
    config.onreset()
  }

  function key(event: KeyboardEvent) {
    const step = event.shiftKey ? BIG_STEP : STEP
    const [back, forward] = config.orientation === 'vertical' ? ['ArrowLeft', 'ArrowRight'] : ['ArrowUp', 'ArrowDown']
    const value = config.value()
    const next = { [back]: value - sign() * step, [forward]: value + sign() * step, Home: config.min(), End: config.max() }[event.key]
    if (next !== undefined) resize(next)
    else if (event.key === 'Enter') reset()
    else return
    event.preventDefault()
  }

  sync()
  node.addEventListener('pointerdown', start)
  node.addEventListener('pointermove', move)
  node.addEventListener('pointerup', end)
  node.addEventListener('pointercancel', end)
  node.addEventListener('dblclick', reset)
  node.addEventListener('keydown', key)

  return {
    update(next: SplitterConfig) {
      config = next
      sync()
    },
    destroy() {
      node.removeEventListener('pointerdown', start)
      node.removeEventListener('pointermove', move)
      node.removeEventListener('pointerup', end)
      node.removeEventListener('pointercancel', end)
      node.removeEventListener('dblclick', reset)
      node.removeEventListener('keydown', key)
    },
  }
}
