<script lang="ts">
  import Notice from '../components/Notice.svelte'
  import ServerProperties from '../components/ServerProperties.svelte'
  import ServerStartup from '../components/ServerStartup.svelte'
  import ServerLaunch from '../components/ServerLaunch.svelte'
  import { onMount, tick } from 'svelte'
  import { openUrl } from '@tauri-apps/plugin-opener'
  import { api, errorText, type ModSide, type Overview, type Override, type PackConfig } from '../lib/api'
  import { serverPack } from '../lib/jobs.svelte'
  import { trackSave } from '../lib/launch.svelte'
  import { serverView } from '../lib/nav.svelte'
  import { commandKey, shortcut } from '../lib/platform'
  import { paneFocus } from '../lib/panes.svelte'

  let { overview, onfiles }: { overview: Overview | null; onfiles: (path: string) => void } = $props()

  type Tab = typeof serverView.tab
  type Settings = Pick<PackConfig['server'], 'memory' | 'jvm_args' | 'args' | 'graalvm' | 'properties' | 'bundle_mods'>

  const tabs: [Tab, string][] = [
    ['mods', 'Mods'],
    ['properties', 'Properties'],
    ['startup', 'Startup'],
  ]

  let mods: ModSide[] = $state([])
  let loadError: string | null = $state(null)
  let loading = $state(true)
  let search = $state('')
  // Names the other mods an override flipped, so a reload doesn't change rows behind your back.
  let changeNote = $state('')
  // The row you just changed stays in view until you pick another filter, so it doesn't vanish under your cursor.
  let pinned = $state<string | null>(null)

  // Quick override changes each start a reload, so we only take the latest one's result.
  let loads = 0
  async function load() {
    const id = ++loads
    loading = true
    try {
      const fresh = await api.serverSides()
      if (id !== loads) return
      mods = fresh
      loadError = null
    } catch (error) {
      if (id === loads) loadError = `Couldn't read mod sides: ${errorText(error)}`
    } finally {
      if (id === loads) loading = false
    }
  }

  let settings = $state<Settings | null>(null)
  let settingsError = $state<string | null>(null)
  let testPort = $state(25565)
  let icon = $state<string | null>(null)
  let launch = $state<{ windows: string; unix: string } | null>(null)
  let saveState = $state<'idle' | 'saving' | 'saved' | 'failed'>('idle')
  let savedCount = $state(0)
  let saveError = $state('')

  async function loadSettings() {
    try {
      const config = await api.getPackConfig()
      const { memory, jvm_args, args, graalvm, properties, bundle_mods } = config.server
      settings = { memory, jvm_args, args, graalvm, properties, bundle_mods }
      testPort = config.test.port
      settingsError = null
    } catch (error) {
      settingsError = `Couldn't read packraven.toml: ${errorText(error)}`
    }
    icon = await api.getServerIcon().catch(() => null)
    launch = await api.serverLaunch().catch(() => null)
  }

  onMount(() => {
    load()
    loadSettings()
  })

  function markChanged() {
    serverPack.changedSinceBuild = true
    serverPack.changedSinceTest = true
  }

  // The Mods tab saves overrides on its own, so each save merges into a fresh read of packraven.toml
  // instead of writing back the config we loaded. Chaining keeps quick edits in order.
  let saving = Promise.resolve()
  function saveSettings(changes: Partial<Settings>) {
    if (!settings) return
    Object.assign(settings, changes)
    saveState = 'saving'
    saving = trackSave(saving.then(async () => {
      try {
        const config = await api.getPackConfig()
        Object.assign(config.server, changes)
        await api.savePackConfig(config)
        markChanged()
        saveState = 'saved'
        savedCount++
      } catch (error) {
        saveError = errorText(error)
        saveState = 'failed'
      }
    }))
  }

  function onTabKey(event: KeyboardEvent) {
    const index = tabs.findIndex(([tab]) => tab === serverView.tab)
    const next = { ArrowRight: index + 1, ArrowLeft: index - 1, Home: 0, End: tabs.length - 1 }[event.key]
    if (next === undefined) return
    event.preventDefault()
    serverView.tab = tabs[(next + tabs.length) % tabs.length][0]
    document.getElementById(`server-tab-${serverView.tab}`)?.focus()
  }

  // We reload every row, since excluding or keeping a mod also decides whether its dependencies go on the server.
  async function setOverride(mod: ModSide, mode: Override, select?: HTMLSelectElement) {
    if (mod.key === null) return
    changeNote = ''
    try {
      await trackSave(api.setSideOverride(mod.key, mode))
      markChanged()
    } catch (error) {
      if (select) select.value = mod.override
      loadError = `Couldn't change the override for ${mod.name}: ${errorText(error)}`
      return
    }
    const wasExcluded = new Map(mods.map((m) => [m.path, m.side === 'client']))
    pinned = mod.path
    await load()
    changeNote = flippedNote(mod.name, mods.filter((m) => m.path !== mod.path && wasExcluded.has(m.path) && wasExcluded.get(m.path) !== (m.side === 'client')))
  }

  // The notice and its button go away once the conflict is fixed, so we move focus to the mod's own row.
  async function keepFromNotice(mod: ModSide) {
    await setOverride(mod, 'keep')
    await tick()
    document.querySelector<HTMLSelectElement>(`select[data-path="${CSS.escape(mod.path)}"]`)?.focus()
  }

  function list(names: string[]) {
    return names.length < 2 ? names.join('') : `${names.slice(0, -1).join(', ')} and ${names.at(-1)}`
  }

  function flippedNote(changed: string, flipped: ModSide[]) {
    const on = flipped.filter((m) => m.side !== 'client').map((m) => m.name)
    const off = flipped.filter((m) => m.side === 'client').map((m) => m.name)
    const parts = [
      on.length ? `${list(on)} ${on.length === 1 ? 'is' : 'are'} now on the server` : '',
      off.length ? `${list(off)} ${off.length === 1 ? 'is' : 'are'} now off ${on.length ? 'it' : 'the server'}` : '',
    ].filter(Boolean)
    return parts.length ? `Because of your change to ${changed}, ${parts.join(', and ')}.` : ''
  }

  function setFilter(next: typeof serverView.filter) {
    serverView.filter = next
    pinned = null
  }

  const breaksServer = (m: ModSide) => m.side === 'client' && m.requiredBy.length > 0
  let conflicts = $derived(mods.filter(breaksServer))
  let excludedCount = $derived(mods.filter((m) => m.side === 'client').length)
  // A crash log names the jar more often than the project, so search matches both.
  let matching = $derived(mods.filter((m) => !search || [m.name, m.path].some((text) => text.toLowerCase().includes(search.toLowerCase()))))
  let visible = $derived(
    matching
      .filter((m) => m.path === pinned || (serverView.filter === 'excluded' ? m.side === 'client' : serverView.filter === 'overridden' ? m.override !== 'auto' : true))
      .sort((a, b) => Number(breaksServer(b)) - Number(breaksServer(a))),
  )

  let searchField = $state<HTMLInputElement>()

  const inFocusedPane = paneFocus()

  function onKey(event: KeyboardEvent) {
    if (!inFocusedPane()) return
    if (serverView.tab !== 'mods' || event.key.toLowerCase() !== 'f' || event.shiftKey || !commandKey(event) || event.defaultPrevented) return
    event.preventDefault()
    searchField?.focus()
    searchField?.select()
  }
</script>

<svelte:window onkeydown={onKey} />

<div class="server">
  <header class="head">
    <div class="title">
      <h2>Server</h2>
      <p class="muted">
        {#if serverView.tab === 'properties'}
          Written to <span class="file">server.properties</span> in every server pack and test run. All other values fall back to vanilla or modded defaults.
        {:else if serverView.tab === 'startup'}
          Choose how hosts get the server pack's mods and how its server starts. We'll write these settings into the pack's <span class="file">user_jvm_args.txt</span>, run scripts and <span class="file">PLEASE READ - IMPORTANT!.txt</span>.
        {:else if loading && !mods.length}
          Reading which mods the server needs.
        {:else if !mods.length}
          This pack has no mods yet.
        {:else}
          {mods.length} {mods.length === 1 ? 'mod' : 'mods'}, {excludedCount || 'none'} excluded from the server. Test runs and Publish build server packs from these choices.
        {/if}
      </p>
    </div>
    <!-- You often restart the test server between override changes, so we offer it here as well as in Run. -->
    <ServerLaunch running={overview?.serverRunning ?? false} />
  </header>

  <div class="tab-row">
    <div class="tabs" role="tablist" aria-label="Server" tabindex="-1" onkeydown={onTabKey}>
      {#each tabs as [tab, label] (tab)}
        <button role="tab" id="server-tab-{tab}" aria-selected={serverView.tab === tab} tabindex={serverView.tab === tab ? 0 : -1} aria-controls="server-panel" class:on={serverView.tab === tab} onclick={() => (serverView.tab = tab)}>
          {label}{#if tab === 'mods' && mods.length}<span class="muted tab-count">{mods.length}</span>{/if}
        </button>
      {/each}
    </div>
    {#if serverView.tab !== 'mods'}
      <!-- The save status sits in the tab row so it can't reflow the header and move the fields you're editing. -->
      <p class="save" class:failed={saveState === 'failed'} role="status">
        {#if saveState === 'saving'}Saving{:else if saveState === 'saved'}Saved to <span class="file">packraven.toml</span>{:else if saveState === 'failed'}Couldn't save: {saveError}{/if}
      </p>
    {:else}
      <input type="search" class="find" bind:this={searchField} bind:value={search} placeholder="Find a mod" aria-label="Find a mod" title="Find a mod ({shortcut('Ctrl+F')})" />
    {/if}
  </div>

  <div class="panel" id="server-panel" role="tabpanel" aria-labelledby="server-tab-{serverView.tab}">
  {#if serverView.tab !== 'mods'}
    {#if settingsError}
      <Notice tone="error">
        <p>{settingsError}</p>
        <p><button onclick={loadSettings}>Try again</button></p>
      </Notice>
    {:else if settings && serverView.tab === 'properties'}
      <ServerProperties
        properties={settings.properties}
        packName={overview?.name ?? 'This pack'}
        version={overview?.version ?? null}
        minecraft={overview?.minecraft ?? null}
        bind:icon
        hasProject={!!overview?.projectId}
        {testPort}
        onchange={(properties) => saveSettings({ properties })}
        oniconchange={markChanged}
      />
    {:else if settings}
      <ServerStartup
        memory={settings.memory}
        jvmArgs={settings.jvm_args}
        args={settings.args}
        graalvm={settings.graalvm}
        bundleMods={settings.bundle_mods}
        {savedCount}
        minecraft={overview?.minecraft ?? null}
        {launch}
        {testPort}
        onchange={saveSettings}
        {onfiles}
      />
    {/if}
  {:else}
  {#if loadError}
    <Notice tone="error">
      <p>{loadError}</p>
      <p><button onclick={() => { loadError = null; load() }} disabled={loading}>Try again</button></p>
    </Notice>
  {/if}

  {#each conflicts as mod (mod.path)}
    <Notice tone="warn">
      <p>You excluded {mod.name}, but {list(mod.requiredBy)} {mod.requiredBy.length === 1 ? 'requires' : 'require'} it, so the server won't start.</p>
      <p><button onclick={() => keepFromNotice(mod)}>Keep {mod.name} on the server</button></p>
    </Notice>
  {/each}

  <div class="tools">
    <div class="filters" role="group" aria-label="Show">
      {#each [['excluded', 'Excluded'], ['overridden', 'Overridden'], ['all', 'All mods']] as const as [id, label] (id)}
        <button aria-pressed={serverView.filter === id} class:on={serverView.filter === id} onclick={() => setFilter(id)}>{label}</button>
      {/each}
    </div>
    <p class="note" role="status">{changeNote}</p>
  </div>

  {#snippet sideChip(mod: ModSide)}
    <span class="chip {breaksServer(mod) ? 'fail' : mod.side === 'client' ? 'change' : 'ok'}">{mod.side === 'client' ? 'Excluded' : mod.side === 'server' ? 'Included, server-side' : 'Included'}</span>
  {/snippet}

  <!-- Past three requirers the reason counts the rest, so we name them all on hover and to screen readers. -->
  {#snippet why(mod: ModSide)}
    {@const all = mod.requiredBy.length > 3 ? `Required by ${list(mod.requiredBy)}` : undefined}
    <span class:muted={!breaksServer(mod)} title={all}><span aria-hidden={all ? 'true' : undefined}>{mod.reason}</span>{#if all}<span class="visually-hidden">{all}</span>{/if}</span>
  {/snippet}

  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div class="table" role="region" aria-label="Mods" tabindex="0">
    <table aria-busy={loading && !mods.length}>
      <thead>
        <tr><th>Mod</th><th class="side">Server</th><th class="why">Why</th><th>Override</th></tr>
      </thead>
      <tbody>
        {#if loading && !mods.length}
          {#each { length: 8 } as _, row (row)}
            <tr class="skeleton" aria-hidden="true">
              <td><span style="width: {55 + ((row * 37) % 35)}%"></span><span class="short"></span></td>
              <td class="side"><span></span></td>
              <td class="why"><span style="width: {60 + ((row * 23) % 30)}%"></span></td>
              <td><span></span></td>
            </tr>
          {/each}
        {:else}
          {#each visible as mod (mod.path)}
            <tr>
              <td>
                {#if mod.url}<a href={mod.url} onclick={(e) => { e.preventDefault(); openUrl(mod.url!) }}>{mod.name}</a>{:else}{mod.name}{/if}
                <div class="file muted">{mod.path.slice(5)}</div>
                <div class="folded">{@render sideChip(mod)} {@render why(mod)}</div>
              </td>
              <td class="side">{@render sideChip(mod)}</td>
              <td class="why">{@render why(mod)}</td>
              <td>
                <select
                  value={mod.override}
                  data-path={mod.path}
                  onchange={(e) => setOverride(mod, e.currentTarget.value as Override, e.currentTarget)}
                  disabled={mod.key === null}
                  title={mod.key === null ? "We can only override mods we matched on CurseForge or Modrinth" : undefined}
                  aria-label="Override for {mod.name}"
                >
                  <option value="auto">Automatic</option>
                  <option value="client">Exclude</option>
                  <option value="keep">Keep on server</option>
                </select>
              </td>
            </tr>
          {:else}
            <tr>
              <td colspan="4" class="muted">
                {#if search && matching.length}
                  No {serverView.filter === 'excluded' ? 'excluded' : 'overridden'} mods match "{search}".
                  <button class="link" onclick={() => setFilter('all')}>{matching.length === 1 ? 'Show the 1 mod that does' : `Show the ${matching.length} mods that do`}</button>
                {:else if search}
                  No mods match "{search}". <button class="link" onclick={() => (search = '')}>Clear the search</button>
                {:else if serverView.filter === 'excluded'}
                  packraven isn't leaving any mods off the server.
                {:else if serverView.filter === 'overridden'}
                  You haven't overridden any mods, so packraven decides them all.
                {:else}
                  This pack has no mods yet.
                {/if}
              </td>
            </tr>
          {/each}
        {/if}
      </tbody>
    </table>
  </div>
  {/if}
  </div>
</div>

<style>
  .server {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-height: 0;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: start;
    justify-content: space-between;
    gap: 8px 24px;
  }

  .title {
    flex: 1 1 20rem;
    min-width: 0;
    display: grid;
    gap: 4px;
  }

  /* Atkinson's characters run narrower than the 0 that ch measures, so 60ch keeps lines near 85 characters. */
  .title p {
    max-width: 60ch;
  }

  .note {
    color: var(--muted);
    font-size: var(--step--1);
  }

  .save {
    align-self: center;
    color: var(--muted);
    font-size: var(--step--1);
  }

  .save.failed {
    color: var(--fail);
  }

  .tab-row {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    justify-content: space-between;
    gap: 8px 16px;
    border-bottom: 1px solid var(--line);
  }

  .tabs {
    display: flex;
    gap: 4px;
  }

  .tabs button {
    margin-bottom: -1px;
    padding: 8px 14px;
    font-weight: 400;
    color: var(--muted);
    background: transparent;
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
  }

  .tabs button:hover:not(.on) {
    color: var(--text);
    background: transparent;
  }

  .tabs button.on {
    color: var(--text);
    font-weight: 700;
    border-bottom-color: var(--forest);
  }

  .tab-count {
    margin-left: 6px;
    font-weight: 400;
    font-variant-numeric: tabular-nums;
  }

  .find {
    flex: 0 1 260px;
    min-width: min(100%, 15rem);
    margin-bottom: 6px;
  }

  .panel {
    container-type: inline-size;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 18px;
    min-height: 0;
  }

  .tools {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
  }

  .filters {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    padding: 3px;
    gap: 3px;
    background: var(--paper);
    border: 1px solid var(--line);
    border-radius: var(--radius-control);
  }

  .filters button {
    font-weight: 400;
    padding: 5px 16px;
    color: var(--muted);
    background: transparent;
    border-color: transparent;
  }

  .filters button:hover:not(.on) {
    color: var(--text);
    background: var(--control-hi);
  }

  .filters button.on {
    color: var(--text);
    font-weight: 700;
    border-color: var(--control-edge);
    background: var(--control);
  }

  .skeleton span {
    display: block;
    height: 0.9em;
    width: 70%;
    margin: 4px 0;
    border-radius: var(--radius-small);
    background: var(--line);
    animation: pulse 1.4s ease-in-out infinite;
  }

  .skeleton span.short {
    width: 35%;
    opacity: 0.6;
  }

  @keyframes pulse {
    50% {
      opacity: 0.45;
    }
  }

  /* We let the table fill the rest of the view, so it scrolls without a page scroll around it. */
  .table {
    flex: 1;
    min-height: 240px;
    overflow: auto;
    border: 1px solid var(--line);
    border-radius: var(--radius-panel);
  }

  table {
    width: 100%;
    border-collapse: collapse;
  }

  th {
    position: sticky;
    top: 0;
    text-align: left;
    font-size: var(--step--1);
    color: var(--muted);
    font-weight: 400;
    background: var(--surface);
    padding: 8px 12px;
  }

  td {
    padding: 8px 12px;
    border-top: 1px solid var(--line);
    vertical-align: top;
  }

  td a {
    color: var(--text);
  }

  td .file {
    overflow-wrap: anywhere;
  }

  .folded {
    display: none;
    margin-top: 4px;
    font-size: var(--step--1);
  }

  /* A narrow table, such as half of a split, folds Server and Why under each mod so Override keeps its room. */
  @container (max-width: 560px) {
    .side,
    .why {
      display: none;
    }

    .folded {
      display: block;
    }
  }

</style>
