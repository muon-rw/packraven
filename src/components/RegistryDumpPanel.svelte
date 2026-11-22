<script lang="ts">
  import { job } from '../lib/jobs.svelte'
  import { StoredSplit, clamp } from '../lib/storedSplit.svelte'
  import JobLog from './JobLog.svelte'
  import Splitter from './Splitter.svelte'

  let {
    stopped,
    maxHeight,
    onstop,
    onretry,
    onclose,
  }: { stopped: boolean; maxHeight: number; onstop: () => void; onretry: () => void; onclose: () => void } = $props()

  const MIN_HEIGHT = 140
  const heightSplit = new StoredSplit('packraven.dumpPanelHeight')
  let max = $derived(Math.max(MIN_HEIGHT, maxHeight))
  let defaultHeight = $derived(clamp(Math.round(window.innerHeight * 0.3), 180, 420))
  let height = $derived(clamp(heightSplit.value ?? defaultHeight, MIN_HEIGHT, max))

  let record = $derived(job('datapack-probe'))
  let running = $derived(record.phase === 'running')

  // Our own progress lines go to the `info` stream, while the server's output goes to `out` and `err`.
  let stage = $derived.by(() => {
    for (let i = record.lines.length - 1; i >= 0; i--) {
      if (record.lines[i].stream === 'info') return record.lines[i].text
    }
    return 'Starting registry dump'
  })

  let now = $state(Date.now())
  $effect(() => {
    if (!running) return
    now = Date.now()
    const timer = setInterval(() => (now = Date.now()), 1000)
    return () => clearInterval(timer)
  })

  let elapsed = $derived.by(() => {
    if (record.startedAt === null) return null
    const seconds = Math.max(0, Math.floor(((record.endedAt ?? now) - record.startedAt) / 1000))
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`
  })

  let summary = $derived.by(() => {
    if (running) return stopped ? 'Stopping the dump server…' : `${stage}…`
    if (record.phase === 'done') return record.message ?? 'Registry dump finished.'
    if (record.phase === 'failed') return record.message ?? "Couldn't dump pack registries."
    return 'No registry dump has run for this pack yet.'
  })
</script>

<section class="dump" id="registry-dump" aria-label="Registry dump" style:height="{height}px">
  <header>
    <p class="summary" class:failed={record.phase === 'failed' && !stopped}>
      <span class="text" role="status">{summary}</span>
      {#if elapsed}<span class="elapsed" title="Elapsed time">{elapsed}</span>{/if}
    </p>
    <span class="buttons">
      {#if running}
        <button class="ghost action" aria-disabled={stopped} title="End the headless dump server without saving a dump" onclick={() => !stopped && onstop()}>Stop</button>
      {:else if record.phase === 'failed'}
        <button class="ghost action" onclick={onretry}>{stopped ? 'Dump again' : 'Try again'}</button>
      {/if}
      <button class="ghost action" title="Hide the dump log (Escape)" onclick={onclose}>{running ? 'Hide' : 'Close'}</button>
    </span>
  </header>
  <JobLog lines={record.lines} label="Registry dump log" empty="The dump log appears here once the server starts." />
</section>
<Splitter
  orientation="horizontal"
  ruled
  value={height}
  min={MIN_HEIGHT}
  {max}
  label="Resize the registry dump log"
  controls="registry-dump"
  onresize={(px) => heightSplit.set(px)}
  onreset={() => heightSplit.reset()}
/>

<style>
  .dump {
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex: none;
    padding: 8px 10px 4px 14px;
  }

  header {
    display: flex;
    align-items: start;
    justify-content: space-between;
    gap: 12px;
  }

  .summary {
    display: flex;
    align-items: baseline;
    gap: 10px;
    min-width: 0;
    margin: 0;
  }

  .summary .text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .summary.failed .text {
    color: var(--warn);
    white-space: normal;
  }

  .elapsed {
    flex: none;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }

  .action {
    padding-inline: 6px;
    color: var(--forest);
    white-space: nowrap;
  }

  .action[aria-disabled='true'] {
    color: var(--muted);
    cursor: default;
  }

  .buttons {
    display: flex;
    flex: none;
    gap: 4px;
  }

  .dump :global(.panel.framed) {
    flex: 1;
    min-height: 0;
  }
</style>
