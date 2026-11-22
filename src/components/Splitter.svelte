<script lang="ts">
  import { splitter } from '../lib/splitter'

  let {
    value,
    min,
    max,
    label,
    controls,
    orientation = 'vertical',
    after = false,
    ruled = false,
    onresize,
    onreset,
  }: {
    value: number
    min: number
    max: number
    label: string
    controls: string
    orientation?: 'vertical' | 'horizontal'
    after?: boolean
    ruled?: boolean
    onresize: (value: number) => void
    onreset: () => void
  } = $props()
</script>

<!-- A focusable separator is interactive under the ARIA window splitter pattern, which Svelte's check doesn't know. -->
<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class:grid={orientation === 'vertical'}
  class:ruled
  use:splitter={{ orientation, after, label, controls, value: () => value, min: () => min, max: () => max, onresize, onreset }}
></div>

<style>
  /* Side-by-side panes lay out as a grid with the splitter in its own column. */
  .grid {
    grid-column: 2;
    grid-row: 1 / -1;
  }
</style>
