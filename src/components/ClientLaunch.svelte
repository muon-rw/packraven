<script lang="ts">
  import Tip from './Tip.svelte'
  import { api, errorText } from '../lib/api'
  import { job, testState } from '../lib/jobs.svelte'
  import { launch as launchState, launchClient, memorySummary } from '../lib/launch.svelte'
  import { minecraft } from '../lib/minecraft.svelte'
  import { nav } from '../lib/nav.svelte'

  let { running }: { running: boolean } = $props()

  let error = $state<string | null>(null)
  // A failure from a launch made in the Run view belongs to that view, so we only report our own.
  let launchedHere = $state(false)

  // Run has its own Join server choice; here we join whenever the test server is up, and the label says so.
  let join = $derived(testState.serverReady)
  let launching = $derived(!running && job('client').phase === 'running')
  let failure = $derived(error ?? (launchedHere && job('client').phase === 'failed' ? job('client').message : null))
  // A warning from a run started in the Run view belongs there, so we only show one for a lone client launch.
  let warning = $derived(launchState.memoryWarning?.launches.every((l) => l.process === 'client') ? launchState.memoryWarning : null)
  let lowMemory = $derived(launchedHere && !failure && warning ? memorySummary(warning) : null)
  let whyNoLaunch = $derived(
    running
      ? 'The client is already running'
      : launching
        ? 'packraven is launching the client'
        : job('mods').phase === 'running'
          ? 'Wait for packraven to finish changing mods'
          : minecraft.account === null
            ? 'Sign in to Minecraft under App settings to launch the client'
            : null,
  )

  async function act(action: () => Promise<void>) {
    error = null
    try {
      await action()
    } catch (e) {
      error = errorText(e)
    }
  }

  function launch() {
    if (whyNoLaunch) return
    launchedHere = true
    act(() => launchClient(join))
  }
</script>

<div class="launch">
  <!-- Kill appears beside Launch rather than in its place, so a second click on Launch can't kill the client. -->
  <Tip tip={whyNoLaunch}>
    {#snippet children(why)}
      <button
        class:busy={launching}
        aria-busy={launching}
        aria-disabled={!!whyNoLaunch}
        aria-describedby={why}
        title={whyNoLaunch ? undefined : join ? 'Launch the client and join the test server' : 'Launch the client to the title screen'}
        onclick={launch}
      >
        {launching ? 'Launching' : join ? 'Launch and join' : 'Launch client'}
      </button>
    {/snippet}
  </Tip>
  {#if running}
    <button class="danger" title="End the client process at once" onclick={() => act(() => api.killProcess('client'))}>Kill client</button>
  {/if}
  <span class="visually-hidden" role="status">{launching ? 'Launching the client' : running ? 'The client is running' : ''}</span>
  {#if failure}
    <p class="failed" role="alert">
      <span class="message" title={failure}>Couldn't launch the client: {failure}</span>
      <button class="link" onclick={() => (nav.view = 'test')}>See the log in Run</button>
    </p>
  {:else if lowMemory}
    <p class="low-memory" role="status">
      <span class="message">{lowMemory}</span>
      <button class="link" onclick={() => (nav.view = 'test')}>See Run</button>
    </p>
  {/if}
</div>

<style>
  .launch {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: end;
    gap: 4px 8px;
  }

  .launch button {
    white-space: nowrap;
  }

  .failed,
  .low-memory {
    flex-basis: 100%;
    text-align: right;
    font-size: var(--step--1);
  }

  .failed {
    color: var(--fail);
  }

  .low-memory {
    color: var(--warn);
  }

  /* The full message is in the Run view's client log, so we keep the header from growing past two lines. */
  .message {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    overflow: hidden;
    max-width: 48ch;
    margin-left: auto;
  }
</style>
