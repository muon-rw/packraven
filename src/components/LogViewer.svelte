<script lang="ts">
  import { untrack } from 'svelte'
  import { api, errorText } from '../lib/api'
  import { LogModel } from '../lib/logs'
  import { kubejsLogFolder } from '../lib/logLinks'
  import LogPanel from './LogPanel.svelte'

  let { path }: { path: string } = $props()

  const POLL_MS = 500

  const model = new LogModel()
  let version = $state(0)
  let generation = $state(0)
  let loadError = $state<string | null>(null)
  let loaded = $state(false)
  let truncated = $state(false)
  let followable = $state(false)
  let offset = 0
  let fetching = false

  export function reload() {
    offset = 0
    return fetchMore()
  }

  async function fetchMore() {
    if (fetching) return
    fetching = true
    const current = path
    try {
      const chunk = await api.readLog(current, offset)
      if (current !== path) return
      if (chunk.reset) {
        model.clear()
        truncated = chunk.truncated
        generation++
      }
      model.append(chunk.text)
      offset = chunk.offset
      followable = chunk.followable
      loadError = null
      loaded = true
      if (chunk.text) version++
    } catch (e) {
      loadError = errorText(e)
    } finally {
      fetching = false
    }
  }

  // Minecraft keeps writing latest.log while the game runs, so we read whatever it added.
  $effect(() => {
    void path
    untrack(() => {
      offset = 0
      loaded = false
      fetchMore()
    })
    const timer = setInterval(() => followable && !document.hidden && fetchMore(), POLL_MS)
    return () => clearInterval(timer)
  })
</script>

<div class="viewer">
  {#if truncated}<p class="note muted">This log is larger than 64 MB, so we show only its end.</p>{/if}
  {#if loadError}<p class="note failed">{loadError}</p>{/if}
  <LogPanel {model} {version} {generation} live={followable} loading={!loaded && !loadError} label="Log lines" empty="This log is empty." scriptFolder={kubejsLogFolder(path)} />
</div>

<style>
  .viewer {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .note {
    padding: 6px 12px;
    font-size: var(--step--1);
    border-bottom: 1px solid var(--line);
  }
</style>
