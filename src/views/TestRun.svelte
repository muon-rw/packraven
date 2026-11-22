<script lang="ts">
  import { onMount, tick } from 'svelte'
  import { openUrl } from '@tauri-apps/plugin-opener'
  import Notice from '../components/Notice.svelte'
  import { api, errorText, type ClientSlot, type KubejsSyncReport, type Overview } from '../lib/api'
  import { SIGN_IN_FIRST, cancelSignIn, minecraft, signIn } from '../lib/minecraft.svelte'
  import { clientJob, job, serverPack, setQuickTestSecondClient, testOptions, testState } from '../lib/jobs.svelte'
  import { killServer, launch, launchClient, memoryWarningText, restartServer, startServer, stopServer } from '../lib/launch.svelte'
  import { appSettingsView, nav, packSettingsView, serverView, showInFiles } from '../lib/nav.svelte'
  import JobLog from '../components/JobLog.svelte'
  import Tip from '../components/Tip.svelte'
  import LinkedText from '../components/LinkedText.svelte'
  import Splitter from '../components/Splitter.svelte'
  import { StoredSplit, clamp } from '../lib/storedSplit.svelte'

  let {
    overview,
    onserver,
    onappsettings,
    onpacksettings,
  }: { overview: Overview | null; onserver: () => void; onappsettings: () => void; onpacksettings: () => void } = $props()

  // The logs are peers, so we keep their ratio rather than a width as the window resizes.
  const LOGS_GAP = 20
  const logSplit = new StoredSplit('packraven.testLogRatio')
  let logsWidth = $state(0)
  let splitTotal = $derived(logsWidth - LOGS_GAP)
  let logMin = $derived(Math.min(220, splitTotal * 0.4))
  let serverWidth = $derived(clamp(splitTotal * (logSplit.value ?? 0.5), logMin, splitTotal - logMin))

  let command = $state('')
  let kubejsReloading = $state(false)
  let kubejsPending = $state<{ startup: boolean; client: boolean } | null>(null)
  let kubejsAnnouncement = $state('')
  let kubejsNoteElement = $state<HTMLElement | null>(null)
  let error: string | null = $state(null)

  const WAIT_FOR_MODS = 'Wait for packraven to finish changing mods'
  const EULA_URL = 'https://aka.ms/MinecraftEULA'
  const SLOTS: ClientSlot[] = ['first', 'second']
  const CLIENT_NAMES: Record<ClientSlot, string> = { first: 'Client', second: 'Second client' }
  // Until settings load we don't know, so we neither ask nor block; start_test still refuses.
  let eulaAccepted: boolean | null = $state(null)
  let account = $derived(minecraft.account)
  let second = $derived(minecraft.second)
  let signInError: string | null = $state(null)
  let quickButton: HTMLButtonElement | undefined = $state()
  onMount(() => {
    run(async () => {
      const settings = await api.getSettings()
      eulaAccepted = settings.minecraftEulaAccepted
    })
  })

  async function startSignIn() {
    signInError = null
    try {
      // The sign-in button goes away once you're signed in, so we hand focus to the action it unblocked.
      if (await signIn()) {
        await tick()
        quickButton?.focus()
      }
    } catch (e) {
      signInError = errorText(e)
    }
  }

  function acceptEula() {
    return run(async () => {
      await api.acceptMinecraftEula()
      eulaAccepted = true
    })
  }

  async function run(action: () => Promise<void>) {
    try {
      error = null
      await action()
    } catch (e) {
      error = errorText(e)
    }
  }

  async function sendCommand() {
    const line = command.trim()
    if (!line || whyNoSend) return
    await run(() => (line === 'stop' ? stopServer() : api.serverCommand(line)))
    command = ''
  }

  const STARTUP_PENDING = "Restart the server to apply your startup script changes, and the game too if it's open."
  const CLIENT_PENDING = 'Press F3+T in the game to load your client script and asset changes.'

  $effect(() => {
    if (!serverRunning) kubejsPending = null
  })

  // Reload only reaches server scripts and data, so we say which of your other edits it left out.
  // We clear the live region first, so the same result read twice is still announced.
  async function reloadKubejs() {
    if (whyNoReloadKubejs) return
    kubejsReloading = true
    kubejsPending = null
    kubejsAnnouncement = ''
    try {
      await run(async () => {
        const report: KubejsSyncReport = await api.reloadKubejsServer()
        const pending = { startup: report.startupChanged, client: report.clientChanged }
        kubejsAnnouncement = [
          'Reloaded KubeJS server scripts and data.',
          pending.startup ? STARTUP_PENDING : '',
          pending.client ? CLIENT_PENDING : '',
        ].filter(Boolean).join(' ')
        if (!pending.startup && !pending.client) return
        kubejsPending = pending
        await tick()
        kubejsNoteElement?.scrollIntoView({ block: 'nearest' })
      })
    } finally {
      kubejsReloading = false
    }
  }

  function openAccounts(anchor: string) {
    appSettingsView.tab = 'accounts'
    nav.anchor = anchor
    onappsettings()
  }

  function openClientMemory() {
    packSettingsView.tab = 'test'
    nav.anchor = 'client-memory'
    onpacksettings()
  }

  function selectTab(slot: ClientSlot) {
    testOptions.clientTab = slot
  }

  function onTabKey(event: KeyboardEvent) {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return
    event.preventDefault()
    const next = event.key === 'Home' ? 'first' : event.key === 'End' ? 'second' : clientTab === 'first' ? 'second' : 'first'
    selectTab(next)
    document.getElementById(`client-tab-${next}`)?.focus()
  }

  let serverRunning = $derived(overview?.serverRunning ?? false)
  let clientRunning = $derived(overview?.clientRunning ?? false)
  let secondRunning = $derived(overview?.secondClientRunning ?? false)
  let preparing = $derived(job('test').phase === 'running')
  let serverLines = $derived(job('test').lines.concat(job('server').lines))
  let running = $derived(serverRunning || clientRunning || secondRunning || preparing)
  let isRunning = (slot: ClientSlot) => (slot === 'first' ? clientRunning : secondRunning)
  // A client's job runs from the moment you launch until its process starts, which for the second
  // client includes copying the instance.
  let isLaunching = (slot: ClientSlot) => !isRunning(slot) && job(clientJob(slot)).phase === 'running'
  let clientState = (slot: ClientSlot) =>
    isRunning(slot) ? ', running' : isLaunching(slot) ? ', launching' : job(clientJob(slot)).phase === 'failed' ? ', failed to launch' : ''
  let joins = (slot: ClientSlot) => (slot === 'first' ? testState.clientJoins : testState.secondClientJoins)

  // Once a run starts we show what the backend resolved; before that, what the next run intends.
  let playerName = $derived((running ? testState.player : null) ?? account?.name ?? null)
  let secondName = $derived((running ? testState.secondPlayer : null) ?? second?.name ?? null)
  let playerIn = (slot: ClientSlot) => (slot === 'first' ? playerName : secondName)
  let serverMode = $derived(
    !running || testState.serverOnline === null
      ? null
      : testState.serverOnline
        ? "This run's server checks each player's account, since online mode is on in server.properties."
        : "This run's server doesn't check accounts, since online mode is off in server.properties.",
  )
  let clientLine = $derived(playerName ? `Logged in as ${playerName} for client runs${secondName ? `, and ${secondName} for the second client` : ''}.` : null)
  let modeLine = $derived([serverMode, clientLine].filter(Boolean).join(' '))
  let clientStatus = $derived.by(() => {
    const live = SLOTS.filter(isRunning)
    if (live.length === 2) {
      return joins('first') && joins('second') ? 'Both clients are launching and will join when they reach the title screen.' : 'Both clients are running.'
    }
    if (!live.length) return null
    const name = live[0] === 'first' ? 'The client' : 'The second client'
    return joins(live[0]) ? `${name} is launching and will join when it reaches the title screen.` : `${name} is running.`
  })
  let status = $derived(
    testState.players.length
      ? `${testState.players.join(', ')} joined the server.`
      : (clientStatus ??
          (testState.serverReady
            ? 'The server is up.'
            : serverRunning
              ? 'The server is starting.'
              : preparing
                ? 'Preparing the test server.'
                : 'Start a test server or client on this computer.')),
  )

  // Quick test only adds the second client once you've signed in its account.
  let withSecond = $derived(testOptions.secondClient && !!second)
  // We keep the second client's tab while it has anything to show, even after you sign it out.
  let showTabs = $derived(!!second || secondRunning || job('second-client').phase !== 'idle')
  let clientTab = $derived<ClientSlot>(showTabs ? testOptions.clientTab : 'first')
  let tabAccount = $derived(clientTab === 'first' ? account : second)
  let tabRunning = $derived(isRunning(clientTab))
  let tabJob = $derived(job(clientJob(clientTab)))
  let tabPlaying = $derived(playerIn(clientTab) ? ` as ${playerIn(clientTab)}` : '')

  let modsChanging = $derived(job('mods').phase === 'running')
  let serverStarting = $derived(preparing || (serverRunning && !testState.serverReady))
  let whyNoEula = $derived(eulaAccepted === false ? 'Accept the Minecraft EULA first' : null)
  let whyNoAccount = $derived(account === null ? SIGN_IN_FIRST : null)
  let whyNoStart = $derived(serverRunning ? 'The test server is already running' : preparing ? 'packraven is preparing the test server' : modsChanging ? WAIT_FOR_MODS : whyNoEula)
  let whyNoStop = $derived(serverRunning ? null : preparing ? "packraven is preparing the test server; you can stop it once it's running" : "The test server isn't running")
  let whyNoQuick = $derived(
    serverRunning
      ? 'The test server is already running. Use Launch client to join it'
      : clientRunning
        ? 'The client is already running'
        : withSecond && (secondRunning || isLaunching('second'))
          ? 'The second client is already running. Kill it, or untick Launch a second client'
          : preparing
            ? 'packraven is preparing the test server'
            : modsChanging
              ? WAIT_FOR_MODS
              : (whyNoEula ?? whyNoAccount),
  )
  let whyNoLaunch = $derived(
    tabRunning
      ? `The ${clientTab === 'first' ? 'client' : 'second client'} is already running`
      : isLaunching(clientTab)
        ? `packraven is launching the ${clientTab === 'first' ? 'client' : 'second client'}`
        : modsChanging
          ? WAIT_FOR_MODS
          : tabAccount === null
            ? clientTab === 'first'
              ? SIGN_IN_FIRST
              : 'Sign in a second Minecraft account under App settings first'
            : testOptions.joinServer && !testState.serverReady
              ? serverStarting
                ? 'Waiting for the test server to finish starting, or untick Join server'
                : 'Start the test server first, or untick Join server'
              : null,
  )
  let whyNoSend = $derived(!serverRunning ? "The test server isn't running" : !command.trim() ? 'Type a command first' : null)
  let whyNoReloadKubejs = $derived(
    !serverRunning
      ? "The test server isn't running"
      : !testState.serverReady
        ? 'Wait for the test server to finish starting'
        : kubejsReloading
          ? 'Syncing and reloading KubeJS'
          : null,
  )
  let recopyHint = $derived(
    serverRunning
      ? 'Stop the test server to change this.'
      : 'Tick this to give each test server a clean slate. Server starts will take 10 to 60 seconds longer.',
  )
  let secondHint = $derived(
    second
      ? `Quick test also launches ${second.name}, from a copy of this instance.`
      : second === null
        ? 'Sign in a second Minecraft account to test with two players.'
        : null,
  )
  let memoryHasServer = $derived(launch.memoryWarning?.launches.some((l) => l.process === 'server') ?? false)
  let memoryHasClient = $derived(launch.memoryWarning?.launches.some((l) => l.process !== 'server') ?? false)
</script>

<div class="test">
  <div class="head">
    <div class="head-info">
      <h2>Run</h2>
      <p class="muted" role="status">{status}</p>
      <p class="muted mode">
        <span aria-live="polite">{modeLine}</span>
        {#if serverMode}
          <button type="button" class="link" onclick={() => ((serverView.tab = 'properties'), onserver())}>Edit server.properties</button>
        {:else if account && !running}
          <button type="button" class="link" onclick={() => openAccounts('minecraft-account-title')}>Manage your sign-in in App settings</button>
        {/if}
      </p>
    </div>
    <div class="controls">
      <div class="options">
        <div class="option">
          <Tip tip={recopyHint}>
            {#snippet children(why)}
              <label class="check">
                <input id="recopy" type="checkbox" bind:checked={testOptions.recopyAll} disabled={serverRunning} aria-describedby={why} />
                Recopy all server files, not just changes
              </label>
            {/snippet}
          </Tip>
        </div>
        <div class="option">
          <Tip tip={secondHint}>
            {#snippet children(why)}
              <label class="check">
                <input
                  id="second-client"
                  type="checkbox"
                  checked={withSecond}
                  onchange={(e) => setQuickTestSecondClient(e.currentTarget.checked)}
                  disabled={!second}
                  aria-describedby={why}
                />
                Launch a second client
              </label>
            {/snippet}
          </Tip>
          {#if second === null}
            <button type="button" class="link" onclick={() => openAccounts('second-account-title')}>Add a second account</button>
          {/if}
        </div>
      </div>
      <Tip tip={whyNoQuick}>
        {#snippet children(why)}
          <button
            class="primary"
            class:busy={preparing}
            bind:this={quickButton}
            aria-disabled={!!whyNoQuick}
            aria-describedby={why}
            title={whyNoQuick ? undefined : withSecond ? 'Start the server, then launch both clients and join it' : 'Start the server, then launch the client and join it'}
            onclick={() => !whyNoQuick && run(() => startServer(true, withSecond))}
          >
            {preparing ? 'Starting' : 'Quick test'}
          </button>
        {/snippet}
      </Tip>
    </div>
  </div>

  <!-- Both steps share one notice, so at the smallest window they don't push the logs out of view. -->
  {#if eulaAccepted === false || account === null}
    <Notice tone="warn">
      <div class="setup">
        {#if eulaAccepted === false}
          <div class="setup-step">
            <p>
              <strong>Accept the Minecraft EULA to start test servers.</strong> Vanilla's server won't run until you do. packraven remembers your answer on this
              computer and marks it accepted in each test server it starts.{account ? ' You can still launch the client on its own.' : ''}
            </p>
            <div class="setup-actions">
              <a href={EULA_URL} onclick={(e) => { e.preventDefault(); openUrl(EULA_URL) }}>Read the Minecraft EULA</a>
              <button class="primary" onclick={acceptEula}>I accept the EULA</button>
            </div>
          </div>
        {/if}
        {#if account === null}
          <div class="setup-step">
            <p>
              <strong>{SIGN_IN_FIRST}.</strong> Mojang only lets launchers start the game for an account that owns it. You can start the test server without
              signing in.
            </p>
            <div class="setup-actions">
              {#if minecraft.signingIn === 'first'}
                <span>Finish signing in with Microsoft in your browser.</span>
                <button type="button" onclick={cancelSignIn}>Cancel</button>
              {:else}
                <button type="button" class="primary" onclick={startSignIn}>Sign in with Microsoft</button>
              {/if}
            </div>
            {#if signInError}<p class="failed" role="alert">{signInError}</p>{/if}
          </div>
        {/if}
      </div>
    </Notice>
  {/if}
  {#if launch.memoryWarning}
    <Notice tone="warn">
      <p>{memoryWarningText(launch.memoryWarning)}</p>
      <p class="notice-actions">
        {#if memoryHasClient}<button type="button" class="link" onclick={openClientMemory}>Change client memory</button>{/if}
        {#if memoryHasServer}<button type="button" class="link" onclick={() => ((serverView.tab = 'startup'), onserver())}>Change server memory</button>{/if}
        <button type="button" class="link" onclick={() => (launch.memoryWarning = null)}>Dismiss</button>
      </p>
    </Notice>
  {/if}
  {#if serverRunning && serverPack.changedSinceTest && !launch.restarting}
    <Notice tone="warn">
      <p>You changed the server pack after the test server started. Restart it to use your changes.</p>
      <p><button onclick={() => run(restartServer)}>Restart server</button></p>
    </Notice>
  {/if}
  {#if launch.exitedOnItsOwn && !serverRunning && !preparing && job('test').phase !== 'failed'}
    <Notice tone="error">
      <p>
        The test server stopped on its own{job('server').message ? ` (${job('server').message?.replace(/^Exited/, 'exited')})` : ''}. Its log is below.
        If a mod crashed it, <button class="link" onclick={() => ((serverView.tab = 'mods'), (serverView.filter = 'all'), onserver())}>leave it off the server</button>.
      </p>
    </Notice>
  {/if}
  {#if error}<Notice tone="error"><p><LinkedText text={error} /></p></Notice>{/if}
  {#if launch.restartError}<Notice tone="error"><p>Couldn't restart the test server: <LinkedText text={launch.restartError} /></p></Notice>{/if}
  {#if job('test').phase === 'failed'}<Notice tone="error"><p><LinkedText text={job('test').message ?? ''} /></p></Notice>{/if}
  {#if job('client').phase === 'failed'}<Notice tone="error"><p>Couldn't launch the client: <LinkedText text={job('client').message ?? ''} /></p></Notice>{/if}
  {#if job('second-client').phase === 'failed'}
    <Notice tone="error"><p>Couldn't launch the second client: <LinkedText text={job('second-client').message ?? ''} /></p></Notice>
  {/if}

  <div class="logs" bind:clientWidth={logsWidth} style:grid-template-columns={logsWidth ? `${serverWidth}px ${LOGS_GAP}px minmax(0, 1fr)` : null}>
    <div class="pane" id="server-pane">
      <div class="pane-head">
        <h3>Server</h3>
        <div class="buttons">
          <Tip tip={whyNoStart}>
            {#snippet children(why)}
              <button aria-disabled={!!whyNoStart} aria-describedby={why} onclick={() => !whyNoStart && run(() => startServer(false))}>Start server</button>
            {/snippet}
          </Tip>
          <Tip tip={whyNoStop ?? 'Save the world and shut down the server'}>
            {#snippet children(why)}
              <button aria-disabled={!!whyNoStop} aria-describedby={why} onclick={() => !whyNoStop && run(stopServer)}>Stop</button>
            {/snippet}
          </Tip>
          <Tip tip={whyNoStop ?? 'End the process at once without saving'}>
            {#snippet children(why)}
              <button class="danger" aria-disabled={!!whyNoStop} aria-describedby={why} onclick={() => !whyNoStop && run(killServer)}>Kill</button>
            {/snippet}
          </Tip>
        </div>
      </div>
      <JobLog lines={serverLines} label="Server log" empty="Start the server to see its log." />
      <form class="console" onsubmit={(e) => { e.preventDefault(); sendCommand() }}>
        <input bind:value={command} placeholder="op {playerName ?? '<name>'}" disabled={!serverRunning} aria-label="Server command" aria-describedby="console-hint" />
        <Tip tip={whyNoSend}>
          {#snippet children(why)}<button type="submit" aria-disabled={!!whyNoSend} aria-describedby={why}>Send</button>{/snippet}
        </Tip>
      </form>
      <div class="hint">
        <p class="muted" id="console-hint">
          {serverRunning ? 'Send commands to the server console. Use Stop to save and shut down, or Kill to end the process without saving.' : 'You can send commands once the server is running.'}
          Server crashing due to a mod? <button class="link" onclick={() => ((serverView.tab = 'mods'), (serverView.filter = 'all'), onserver())}>Leave it off the server</button>.
        </p>
        {#if overview?.kubejsInstalled || overview?.kubejsLogs.length}
          <div class="kubejs-bar">
            {#if overview?.kubejsInstalled}
              <Tip tip={whyNoReloadKubejs ?? 'Copy changed kubejs/server_scripts and kubejs/data to the test server and reload them'}>
                {#snippet children(why)}
                  <button type="button" class="kubejs-reload" class:busy={kubejsReloading} aria-disabled={!!whyNoReloadKubejs} aria-describedby={why} onclick={reloadKubejs}>
                    {kubejsReloading ? 'Reloading KubeJS' : 'Reload KubeJS'}
                  </button>
                {/snippet}
              </Tip>
            {/if}
            {#if overview?.kubejsLogs.length}
              <span class="kubejs-logs muted">
                KubeJS logs:
                {#each overview.kubejsLogs as logPath, index (logPath)}
                  {#if index > 0}<span aria-hidden="true">·</span>{/if}
                  <button type="button" class="link" onclick={() => showInFiles(logPath)}>{logPath.slice(logPath.lastIndexOf('/') + 1)}</button>
                {/each}
              </span>
            {/if}
            <p class="visually-hidden" role="status">{kubejsAnnouncement}</p>
            {#if kubejsPending}
              <div class="kubejs-note" bind:this={kubejsNoteElement}>
                {#if kubejsPending.startup}
                  <p>{STARTUP_PENDING} <button type="button" class="link" onclick={() => run(restartServer)}>Restart server</button></p>
                {/if}
                {#if kubejsPending.client}<p>{CLIENT_PENDING}</p>{/if}
              </div>
            {/if}
          </div>
        {/if}
      </div>
    </div>

    <Splitter value={serverWidth} min={logMin} max={splitTotal - logMin} label="Resize the server and client logs" controls="server-pane" onresize={(px) => logSplit.set(px / splitTotal)} onreset={() => logSplit.reset()} />

    <div class="pane">
      <div class="pane-head">
        {#if showTabs}
          <div class="client-tabs" role="tablist" aria-label="Clients" tabindex="-1" onkeydown={onTabKey}>
            {#each SLOTS as slot (slot)}
              <button
                type="button"
                role="tab"
                id="client-tab-{slot}"
                aria-selected={clientTab === slot}
                aria-controls="client-panel"
                tabindex={clientTab === slot ? 0 : -1}
                class:on={clientTab === slot}
                aria-label={CLIENT_NAMES[slot] + clientState(slot)}
                onclick={() => selectTab(slot)}
              >
                {CLIENT_NAMES[slot]}
                {#if isRunning(slot) || isLaunching(slot)}
                  <span class="live" class:launching={!isRunning(slot)} aria-hidden="true"></span>
                {:else if job(clientJob(slot)).phase === 'failed'}
                  <span class="live failed" aria-hidden="true"></span>
                {/if}
              </button>
            {/each}
          </div>
        {:else}
          <h3>Client</h3>
        {/if}
        <div class="buttons">
          <Tip tip={whyNoLaunch}>
            {#snippet children(why)}
              <button aria-disabled={!!whyNoLaunch} aria-describedby={why} onclick={() => !whyNoLaunch && run(() => launchClient(testOptions.joinServer, clientTab))}>
                {clientTab === 'first' ? 'Launch client' : 'Launch second client'}
              </button>
            {/snippet}
          </Tip>
          <Tip tip={tabRunning ? 'End the client process at once' : "The client isn't running"}>
            {#snippet children(why)}
              <button class="danger" aria-disabled={!tabRunning} aria-describedby={why} onclick={() => tabRunning && run(() => api.killProcess(clientJob(clientTab)))}>Kill</button>
            {/snippet}
          </Tip>
        </div>
      </div>
      <div class="client-log" id="client-panel" role={showTabs ? 'tabpanel' : undefined} aria-labelledby={showTabs ? `client-tab-${clientTab}` : undefined}>
        <!-- The log keeps its own copy of the lines it has seen, so each client gets a fresh one. -->
        {#key clientTab}
          <JobLog
            lines={tabJob.lines}
            label={clientTab === 'first' ? 'Client log' : 'Second client log'}
            empty={clientTab === 'first' ? 'The client log will appear here after you launch it.' : "The second client's log will appear here after you launch it."}
          />
        {/key}
      </div>
      <div class="client-bar">
        <label class="check join">
          <!-- Quick test always joins, so while a client runs we show what that run did. -->
          <input type="checkbox" checked={tabRunning ? joins(clientTab) : testOptions.joinServer} onchange={(e) => (testOptions.joinServer = e.currentTarget.checked)} disabled={tabRunning || tabAccount === null} aria-describedby="client-hint" /> Join server
        </label>
      </div>
      <p class="hint muted" id="client-hint">
        {#if tabRunning}
          Running{tabPlaying}{joins(clientTab) ? ' and joining the test server' : ', without joining the test server'}.
        {:else if tabAccount === null && clientTab === 'first'}
          {SIGN_IN_FIRST}.
        {:else if tabAccount === null}
          Sign in a second Minecraft account to launch this client.
          <button type="button" class="link" onclick={() => openAccounts('second-account-title')}>Add a second account</button>
        {:else}
          {clientTab === 'first' ? 'Launch this instance' : 'Launch a copy of this instance'}{tabPlaying}{!testOptions.joinServer
            ? ' to the title screen.'
            : testState.serverReady
              ? ' and join the test server.'
              : '. Start the server first, or untick Join server to launch to the title screen.'}
        {/if}
      </p>
    </div>
  </div>
</div>

<style>
  .test {
    display: flex;
    flex-direction: column;
    gap: 18px;
    flex: 1;
    min-height: 480px;
  }

  .options {
    display: grid;
    gap: 6px;
  }

  .option {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 10px;
  }

  .option .link {
    font-size: var(--step--1);
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: start;
    gap: 12px 24px;
  }

  .head-info {
    flex: 1 1 24ch;
    min-width: 0;
  }

  .head p {
    margin-top: 4px;
  }

  .head .mode {
    margin-top: 2px;
    font-size: var(--step--1);
  }

  .setup {
    display: grid;
    gap: 20px;
  }

  .setup-step {
    display: grid;
    gap: 10px;
  }

  .setup-actions,
  .notice-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 16px;
  }

  .controls {
    display: flex;
    align-items: center;
    gap: 12px 18px;
    flex-wrap: wrap;
    justify-content: end;
  }

  /* We share rows across the panes, so both logs start level when the client's buttons wrap. */
  .logs {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) 20px minmax(0, 1fr);
    /* The floor keeps each log's toolbar clear of the console when notices take the space; the view scrolls instead. */
    grid-template-rows: auto minmax(220px, 1fr) auto auto;
  }

  .pane {
    display: grid;
    grid-row: span 4;
    grid-template-rows: subgrid;
    grid-template-columns: minmax(0, 1fr);
    row-gap: 10px;
    min-height: 0;
  }

  .pane > .hint {
    grid-row: 4;
  }

  .client-log {
    display: grid;
    grid-template-rows: minmax(0, 1fr);
    grid-template-columns: minmax(0, 1fr);
    min-height: 0;
  }

  .client-bar {
    display: flex;
    align-items: center;
  }

  .pane-head,
  .buttons,
  .console {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }

  .pane-head,
  .buttons {
    flex-wrap: wrap;
  }

  .buttons {
    justify-content: end;
  }

  /* The tabs stand in for the pane's heading, so they take its size and weight. */
  .client-tabs {
    display: flex;
    gap: 4px;
    margin-left: -10px;
  }

  .client-tabs button {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    padding: 4px 10px;
    font-size: var(--step-1);
    font-weight: 400;
    color: var(--muted);
    background: transparent;
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    white-space: nowrap;
  }

  .client-tabs button:hover:not(.on) {
    color: var(--text);
    background: transparent;
  }

  .client-tabs button.on {
    color: var(--text);
    font-weight: 700;
    border-bottom-color: var(--forest);
  }

  .live {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--forest);
  }

  .live.launching {
    background: var(--busy);
  }

  .live.failed {
    background: var(--fail);
  }

  .join {
    white-space: nowrap;
  }

  .buttons button {
    white-space: nowrap;
  }

  .hint {
    font-size: var(--step--1);
  }

  .kubejs-bar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 12px;
    margin-top: 8px;
  }

  .kubejs-reload {
    padding: 3px 10px;
    font-size: var(--step--1);
    white-space: nowrap;
  }

  .kubejs-note {
    flex-basis: 100%;
    font-size: var(--step--1);
    color: var(--warn);
  }

  .kubejs-note p {
    margin: 0;
  }

  .kubejs-logs {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0 5px;
  }

  .console input {
    flex: 1;
    font-family: var(--font-log);
    font-size: var(--step--1);
  }
</style>
