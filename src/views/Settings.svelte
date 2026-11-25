<script lang="ts">
  import Notice from '../components/Notice.svelte'
  import CredentialField from '../components/CredentialField.svelte'
  import JavaPicker from '../components/JavaPicker.svelte'
  import ExtraLaunchArgs, { emptyDraft, fromDraft, toDraft } from '../components/ExtraLaunchArgs.svelte'
  import SaveBadge from '../components/SaveBadge.svelte'
  import { onDestroy, onMount, tick } from 'svelte'
  import { open } from '@tauri-apps/plugin-dialog'
  import { disable, enable, isEnabled } from '@tauri-apps/plugin-autostart'
  import { openUrl } from '@tauri-apps/plugin-opener'
  import { api, errorText, type ClientSlot, type CredentialsView, type DefenderView, type RunArgs, type Settings, type UpdateView } from '../lib/api'
  import { installNow, openUpdateChangelog, selfUpdate, updateWaiting } from '../lib/selfUpdate.svelte'
  import { javas, scanJava } from '../lib/java.svelte'
  import { trackSave } from '../lib/launch.svelte'
  import { accountIn, cancelSignIn, minecraft, signIn, signOut } from '../lib/minecraft.svelte'
  import { appSettingsView } from '../lib/nav.svelte'

  let { onchange }: { onchange: () => void } = $props()

  const tabs = [
    ['general', 'General'],
    ['java', 'Java & Test runs'],
    ['accounts', 'Accounts & Tokens'],
  ] as const

  function selectTab(tab: (typeof tabs)[number][0]) {
    flushPending()
    appSettingsView.tab = tab
  }

  let lastTab = appSettingsView.tab
  $effect.pre(() => {
    const next = appSettingsView.tab
    if (next !== lastTab) {
      flushPending()
      lastTab = next
    }
  })

  function onTabKey(event: KeyboardEvent) {
    const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0
    if (!step && event.key !== 'Home' && event.key !== 'End') return
    event.preventDefault()
    const index = tabs.findIndex(([tab]) => tab === appSettingsView.tab)
    const next = event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : (index + step + tabs.length) % tabs.length
    selectTab(tabs[next][0])
    document.getElementById(`app-settings-tab-${tabs[next][0]}`)?.focus()
  }

  let settings: Settings | null = $state(null)
  let savedSettings: Settings | null = $state(null)
  let editorAskDraft = $state<number | null>(50)
  let editorMaxDraft = $state<number | null>(100)
  let autostart = $state(false)
  let credentials = $state<CredentialsView | null>(null)
  let envNames = $state<string[]>([])
  let settingsError: string | null = $state(null)
  let autostartError: string | null = $state(null)
  let credentialsError: string | null = $state(null)
  let savedField = $state<string | null>(null)
  let saveState = $state<'idle' | 'saving' | 'saved' | 'failed'>('idle')
  let saveTick = $state(0)
  let saveError = $state<string | null>(null)
  let runArgs = $state(emptyDraft())
  let savedRunArgs = $state<RunArgs>(fromDraft(emptyDraft()))

  let editorAskInvalid = $derived(editorAskDraft === null || !Number.isInteger(editorAskDraft) || editorAskDraft < 1)
  let editorMaxBelowAsk = $derived.by(() => {
    const current = settings
    return current !== null && editorMaxDraft !== null && Number.isInteger(editorMaxDraft) && editorMaxDraft >= 1 && editorMaxDraft < current.editorAskMb
  })
  let editorMaxInvalid = $derived(editorMaxDraft === null || !Number.isInteger(editorMaxDraft) || editorMaxDraft < 1 || editorMaxBelowAsk)

  async function load() {
    try {
      const loaded = await api.getSettings()
      runArgs = toDraft(loaded.runArgs)
      savedRunArgs = fromDraft(runArgs)
      editorAskDraft = loaded.editorAskMb
      editorMaxDraft = loaded.editorMaxMb
      settings = loaded
      savedSettings = structuredClone(loaded)
      settingsError = null
    } catch (error) {
      settingsError = errorText(error)
      return
    }
    await Promise.all([loadCredentials(), loadAccount()])
  }

  type AccountUi = { error: { text: string; retry: () => void } | null; status: string; button?: HTMLButtonElement }
  // We keep one live region per account and change its text, since screen readers often miss a status that appears.
  const accountUi = $state<Record<ClientSlot, AccountUi>>({ first: { error: null, status: '' }, second: { error: null, status: '' } })

  // Each state swaps the button you just pressed for another, so we move focus to the new one.
  async function focusAccountButton(slot: ClientSlot) {
    await tick()
    accountUi[slot].button?.focus()
  }

  async function loadAccount() {
    try {
      const accounts = await api.minecraftAccounts()
      minecraft.account = accounts.first
      minecraft.second = accounts.second
      accountUi.first.error = null
    } catch (error) {
      accountUi.first.error = { text: `Couldn't read your Minecraft sign-ins: ${errorText(error)}`, retry: loadAccount }
    }
  }

  async function startSignIn(slot: ClientSlot) {
    const ui = accountUi[slot]
    ui.error = null
    ui.status = 'Waiting for you to sign in with Microsoft in your browser.'
    const signingIn = signIn(slot)
    focusAccountButton(slot)
    try {
      const signedIn = await signingIn
      ui.status = signedIn ? `Signed in as ${signedIn.name}.` : 'Sign-in cancelled.'
    } catch (error) {
      ui.status = ''
      ui.error = { text: errorText(error), retry: () => startSignIn(slot) }
    } finally {
      focusAccountButton(slot)
    }
  }

  async function startSignOut(slot: ClientSlot) {
    const ui = accountUi[slot]
    ui.error = null
    try {
      await signOut(slot)
      ui.status = slot === 'first' ? 'Signed out of Minecraft.' : 'Signed out of the second account.'
      focusAccountButton(slot)
    } catch (error) {
      ui.error = { text: errorText(error), retry: () => startSignOut(slot) }
    }
  }

  async function loadCredentials() {
    try {
      ;[credentials, envNames] = await Promise.all([api.credentialsStatus(), api.envVarNames()])
      credentialsError = null
    } catch (error) {
      credentials = null
      credentialsError = errorText(error)
    }
  }

  const UPLOAD_TOKEN_PAGE = 'https://legacy.curseforge.com/account/api-tokens'
  const GITHUB_TOKEN_PAGE = 'https://github.com/settings/personal-access-tokens/new'
  const MODRINTH_TOKEN_PAGE = 'https://modrinth.com/settings/pats'

  // Other views and background checks update settings.json too, so each save merges only the changed
  // field into a fresh read and chains in order.
  let saving = Promise.resolve()

  function cleanJavaByMajor(javaByMajor: Record<string, string>): Record<string, string> {
    return Object.fromEntries(
      Object.entries(javaByMajor)
        .map(([major, java]) => [major, java.trim()])
        .filter(([, java]) => java),
    )
  }

  function runSave(field: string, action: () => Promise<void>, after?: () => void) {
    savedField = field
    saveState = 'saving'
    saveError = null
    saving = trackSave(
      saving.then(async () => {
        try {
          await action()
          after?.()
          onchange()
          if (savedField === field) {
            saveTick += 1
            saveState = 'saved'
          }
        } catch (error) {
          saveError = `Couldn't save: ${errorText(error)}`
          if (savedField === field) saveState = 'failed'
        }
      }),
    )
  }

  function saveSettingsField(field: string, apply: (target: Settings) => void, after?: () => void) {
    if (!settings || !savedSettings) return
    apply(settings)
    apply(savedSettings)
    runSave(
      field,
      async () => {
        const fresh = await api.getSettings()
        apply(fresh)
        fresh.javaByMajor = cleanJavaByMajor(fresh.javaByMajor)
        await api.saveSettings(fresh)
      },
      after,
    )
  }

  const timers = new Map<string, { timer: ReturnType<typeof setTimeout>; commit: () => void }>()

  function clearEdit(field: string) {
    const pending = timers.get(field)
    if (pending) {
      clearTimeout(pending.timer)
      timers.delete(field)
    }
  }

  function scheduleSave(field: string, commit: () => void) {
    clearEdit(field)
    if (saveState !== 'failed') saveState = 'idle'
    const timer = setTimeout(() => {
      timers.delete(field)
      commit()
    }, 400)
    timers.set(field, { timer, commit })
  }

  function flushField(field: string, commit: () => void) {
    clearEdit(field)
    commit()
  }

  function flushPending() {
    for (const { timer, commit } of timers.values()) {
      clearTimeout(timer)
      commit()
    }
    timers.clear()
  }

  onDestroy(flushPending)

  function onFormKey(event: KeyboardEvent) {
    if (event.key === 'Enter' && event.target instanceof HTMLInputElement && event.target.type !== 'checkbox' && event.target.type !== 'button') {
      event.preventDefault()
      event.target.dispatchEvent(new Event('change', { bubbles: true }))
    }
  }

  function saveInstallDir() {
    if (!settings || !savedSettings || settings.installDir === savedSettings.installDir) return
    const next = settings.installDir
    saveSettingsField('install-dir', (s) => (s.installDir = next), () => scanJava())
  }

  function saveGitName() {
    if (!settings || !savedSettings || settings.gitName === savedSettings.gitName) return
    const next = settings.gitName
    identityMessage = null
    saveSettingsField('git-name', (s) => (s.gitName = next))
  }

  function saveGitEmail() {
    if (!settings || !savedSettings || settings.gitEmail === savedSettings.gitEmail) return
    const next = settings.gitEmail
    identityMessage = null
    saveSettingsField('git-email', (s) => (s.gitEmail = next))
  }

  function saveJavaSlot(major: string) {
    if (!settings || !savedSettings) return
    const next = (settings.javaByMajor[major] ?? '').trim()
    const prev = (savedSettings.javaByMajor[major] ?? '').trim()
    if (next === prev) return
    saveSettingsField(
      `java-${major}`,
      (s) => {
        if (next) s.javaByMajor[major] = next
        else delete s.javaByMajor[major]
      },
      () => scanJava(),
    )
  }

  // The backend saves Java choices itself when you download GraalVM: it chooses GraalVM 21 for
  // Java 17, and an update moves every setting off the build it replaces.
  function reloadJavaPaths() {
    saving = trackSave(
      saving.then(async () => {
        try {
          const fresh = await api.getSettings()
          if (settings) settings.javaByMajor = { ...fresh.javaByMajor }
          if (savedSettings) savedSettings.javaByMajor = { ...fresh.javaByMajor }
          onchange()
        } catch (error) {
          saveError = `Couldn't reload your Java settings: ${errorText(error)}`
        }
      }),
    )
  }

  function saveRunArgs(key: keyof RunArgs) {
    if (!settings || !savedSettings) return
    const next = fromDraft(runArgs)
    if (JSON.stringify(next) === JSON.stringify(savedRunArgs)) return
    savedRunArgs = next
    saveSettingsField(key, (s) => (s.runArgs = next))
  }

  function saveEditorAsk() {
    if (!savedSettings || editorAskInvalid || editorAskDraft === null || editorAskDraft === savedSettings.editorAskMb) return
    const next = editorAskDraft
    if (editorMaxDraft === null || editorMaxDraft < next) editorMaxDraft = next
    saveSettingsField('editor-ask', (s) => {
      s.editorAskMb = next
      if (s.editorMaxMb < next) s.editorMaxMb = next
    })
  }

  function saveEditorMax() {
    if (!savedSettings || editorMaxInvalid || editorMaxDraft === null || editorMaxDraft === savedSettings.editorMaxMb) return
    const next = editorMaxDraft
    saveSettingsField('editor-max', (s) => (s.editorMaxMb = next))
  }

  let identityBusy = $state(false)
  let identityMessage = $state<{ ok: boolean; text: string } | null>(null)

  async function fillIdentityFromGithub() {
    if (!settings || !savedSettings) return
    identityBusy = true
    clearEdit('git-name')
    clearEdit('git-email')
    try {
      const identity = await api.githubIdentity()
      settings.gitName = identity.name
      settings.gitEmail = identity.email
      savedSettings.gitName = identity.name
      savedSettings.gitEmail = identity.email
      await (saving = trackSave(
        saving.then(async () => {
          const fresh = await api.getSettings()
          fresh.gitName = identity.name
          fresh.gitEmail = identity.email
          fresh.javaByMajor = cleanJavaByMajor(fresh.javaByMajor)
          await api.saveSettings(fresh)
          onchange()
        }),
      ))
      identityMessage = { ok: true, text: 'Filled in from your GitHub account.' }
    } catch (error) {
      identityMessage = { ok: false, text: `Couldn't read your GitHub account: ${errorText(error)}` }
    } finally {
      identityBusy = false
    }
  }

  onMount(async () => {
    scanJava()
    await load()
    autostart = await isEnabled().catch(() => false)
  })

  async function pickInstall() {
    const chosen = await open({ directory: true, defaultPath: settings?.installDir })
    if (typeof chosen === 'string' && settings) {
      settings.installDir = chosen
      flushField('install-dir', saveInstallDir)
    }
  }

  const JAVA_SLOTS = [
    { major: '17', versions: 'Minecraft 1.18 to 1.20.4 (Java 17)' },
    { major: '21', versions: 'Minecraft 1.20.5 to 1.21 (Java 21)' },
    { major: '25', versions: 'Minecraft 26.1 and later (Java 25)' },
  ]

  async function toggleAutostart() {
    const wanted = !autostart
    autostart = wanted
    savedField = 'autostart'
    saveState = 'saving'
    try {
      autostartError = null
      if (wanted) await enable()
      else await disable()
      if (savedField === 'autostart') {
        saveTick += 1
        saveState = 'saved'
      }
    } catch (error) {
      autostart = !wanted
      if (savedField === 'autostart') saveState = 'failed'
      autostartError = `Couldn't change the startup setting: ${errorText(error)}`
    }
  }

  let autoUpdateError: string | null = $state(null)
  let checkingUpdate = $state(false)
  let discordPresenceError: string | null = $state(null)
  let discordShowsPackError: string | null = $state(null)

  function toggleSettingSwitch(
    field: string,
    key: 'autoUpdate' | 'discordPresence' | 'discordShowsPack',
    save: (enabled: boolean) => Promise<void>,
    setError: (message: string | null) => void,
    errorPrefix: string,
  ) {
    if (!settings) return
    const wanted = !settings[key]
    settings[key] = wanted
    savedField = field
    saveState = 'saving'
    setError(null)
    saving = trackSave(
      saving.then(async () => {
        try {
          await save(wanted)
          if (savedSettings) savedSettings[key] = wanted
          if (savedField === field) {
            saveTick += 1
            saveState = 'saved'
          }
        } catch (error) {
          if (settings) settings[key] = savedSettings ? savedSettings[key] : !wanted
          if (savedField === field) saveState = 'failed'
          setError(`${errorPrefix}: ${errorText(error)}`)
        }
      }),
    )
  }

  function toggleAutoUpdate() {
    toggleSettingSwitch('auto-update', 'autoUpdate', (enabled) => api.setAutoUpdate(enabled), (e) => (autoUpdateError = e), "Couldn't change the update setting")
  }

  function toggleDiscordPresence() {
    toggleSettingSwitch('discord-presence', 'discordPresence', (enabled) => api.setDiscordPresence(enabled), (e) => (discordPresenceError = e), "Couldn't change the Discord setting")
  }

  function toggleDiscordShowsPack() {
    toggleSettingSwitch('discord-shows-pack', 'discordShowsPack', (enabled) => api.setDiscordShowsPack(enabled), (e) => (discordShowsPackError = e), "Couldn't change the Discord setting")
  }

  async function checkForUpdate() {
    checkingUpdate = true
    try {
      selfUpdate.view = await api.checkForUpdate()
    } catch {
      // The backend reports the failure as the update status too, so the status line shows it.
    } finally {
      checkingUpdate = false
    }
  }

  function updateStatusText(view: UpdateView): string {
    if (!view.supported) return `This is a development build of packraven ${view.current}, so we don't update it.`
    switch (view.status.phase) {
      case 'unchecked':
        return `You have packraven ${view.current}.`
      case 'checking':
        return 'Checking for updates.'
      case 'upToDate':
        return `packraven ${view.current} is the latest version.`
      case 'available':
        return `packraven ${view.status.version} is available. You have ${view.current}.`
      case 'downloading':
        return `Downloading packraven ${view.status.version}.`
      case 'ready':
        return view.auto
          ? `packraven ${view.status.version} is ready. We'll install it when you quit packraven, or you can restart now.`
          : `packraven ${view.status.version} is ready. Restart packraven to install it.`
      case 'failed':
        if (view.status.stage === 'install') return "Couldn't install the update."
        // Background checks fail whenever you're offline, so we keep these calm and retry later.
        return `Couldn't ${view.status.stage === 'check' ? 'check for updates' : 'download the update'}, so we'll try again later. You have packraven ${view.current}.`
    }
  }

  let defender = $state<DefenderView | null>(null)
  let defenderBusy = $state(false)
  let defenderMessage = $state<string | null>(null)
  let defenderFailed = $state(false)

  onMount(async () => {
    defender = await api.defenderStatus().catch(() => null)
  })

  async function excludeFolders() {
    if (!defender) return
    defenderBusy = true
    try {
      await api.defenderExclude(defender.missing)
      defender = await api.defenderStatus()
      defenderFailed = false
      defenderMessage = 'Folders excluded.'
    } catch (error) {
      defenderFailed = true
      defenderMessage = `Couldn't exclude the folders: ${errorText(error)}`
    } finally {
      defenderBusy = false
    }
  }

  async function setDefenderPrompt(enabled: boolean, box: HTMLInputElement) {
    savedField = 'defender-prompt'
    saveState = 'saving'
    try {
      await api.defenderDismiss(!enabled)
      if (settings) settings.defenderDismissed = !enabled
      if (savedSettings) savedSettings.defenderDismissed = !enabled
      if (savedField === 'defender-prompt') {
        saveTick += 1
        saveState = 'saved'
      }
    } catch (error) {
      box.checked = !enabled
      if (savedField === 'defender-prompt') saveState = 'failed'
      defenderFailed = true
      defenderMessage = `Couldn't change the reminder: ${errorText(error)}`
    }
  }
</script>

{#snippet badge(field: string)}
  <SaveBadge active={savedField === field} state={saveState} tick={saveTick} />
{/snippet}

{#if settingsError}
  <Notice tone="error">
    <p>Couldn't read packraven's settings: {settingsError}</p>
    <p><button onclick={load}>Try again</button></p>
  </Notice>
{:else if settings}
  <div class="settings">
  <header>
    <h2>App settings</h2>
    <p class="muted">Configure general packraven settings. For individual packs, see Pack settings.</p>
  </header>

  <div class="tab-row">
    <div class="tabs" role="tablist" aria-label="App settings" onkeydown={onTabKey}>
      {#each tabs as [tab, label] (tab)}
        <button
          type="button"
          role="tab"
          id="app-settings-tab-{tab}"
          aria-selected={appSettingsView.tab === tab}
          tabindex={appSettingsView.tab === tab ? 0 : -1}
          aria-controls="app-settings-panel"
          class:on={appSettingsView.tab === tab}
          onclick={() => selectTab(tab)}
        >
          {label}
        </button>
      {/each}
    </div>
  </div>

  <div class="panel" id="app-settings-panel" role="tabpanel" aria-labelledby="app-settings-tab-{appSettingsView.tab}">
  {#if appSettingsView.tab === 'accounts'}
  <section class="account" id="minecraft-account" aria-labelledby="minecraft-account-title">
    <h3 id="minecraft-account-title" tabindex="-1">Minecraft account</h3>
    {#if minecraft.account}
      <p>
        Signed in as <strong>{minecraft.account.name}</strong>. Test clients on this computer launch as this account.
      </p>
    {:else if minecraft.signingIn === 'first'}
      <p class="muted">Finish signing in with Microsoft in your browser. packraven is waiting.</p>
    {:else}
      <p class="muted">
        Mojang only lets launchers start the game for an account that owns it, so sign in with a Microsoft account that owns Minecraft: Java Edition to
        launch test clients. You can start test servers without signing in.
      </p>
    {/if}
    {@render accountControls('first')}
  </section>

  <section class="account" id="second-minecraft-account" aria-labelledby="second-account-title">
    <h3 id="second-account-title" tabindex="-1">Second account</h3>
    {#if minecraft.second}
      <p>
        Signed in as <strong>{minecraft.second.name}</strong>. Run launches the second client as this account, from a copy of the pack's instance with its own
        settings and worlds.
      </p>
    {:else if minecraft.signingIn === 'second'}
      <p class="muted">
        Finish signing in with Microsoft in your browser. If Microsoft offers {minecraft.account?.name ?? 'your main account'}, choose Use another account.
      </p>
    {:else}
      <p class="muted">
        To test with two players, sign in with a second Microsoft account that owns Minecraft: Java Edition. Run can then launch a second client as that account,
        from a copy of the pack's instance with its own settings and worlds.
      </p>
    {/if}
    {@render accountControls('second')}
    <p class="muted hint">packraven is not an official Minecraft product, and is not approved by or associated with Mojang or Microsoft.</p>
  </section>
  {/if}

  {#snippet accountControls(slot: ClientSlot)}
    {@const ui = accountUi[slot]}
    {@const other = minecraft.signingIn !== null && minecraft.signingIn !== slot}
    <div class="identity-row">
      {#if accountIn(slot)}
        <button type="button" bind:this={ui.button} onclick={() => startSignOut(slot)}>Sign out</button>
      {:else if minecraft.signingIn === slot}
        <button type="button" bind:this={ui.button} onclick={cancelSignIn}>Cancel</button>
      {:else}
        <!-- Starting a second sign-in cancels the one in progress, so we wait for it instead. -->
        <button
          type="button"
          class="primary"
          bind:this={ui.button}
          aria-disabled={other}
          aria-describedby={other ? `${slot}-sign-in-wait` : undefined}
          onclick={() => !other && startSignIn(slot)}>Sign in with Microsoft</button
        >
        {#if other}<span class="muted hint" id="{slot}-sign-in-wait">Finish or cancel the other sign-in first.</span>{/if}
      {/if}
    </div>
    <p class="visually-hidden" role="status">{ui.status}</p>
    {#if ui.error}
      <Notice tone="error">
        <p>{ui.error.text}</p>
        <p><button type="button" onclick={ui.error.retry}>Try again</button></p>
      </Notice>
    {/if}
  {/snippet}

  {#if appSettingsView.tab === 'general'}
  <div class="app-switches">
  <div class="switch-row">
    <div>
      <div class="switch-label">
        <label for="autostart-switch" id="autostart-label">Start in the tray when I sign in to this computer</label>
        {@render badge('autostart')}
      </div>
      <p class="muted" id="autostart-hint">Keep packraven running so builds and test servers are a click away.</p>
      {#if autostartError}<p class="failed" id="autostart-error" role="alert">{autostartError}</p>{/if}
    </div>
    <button
      id="autostart-switch"
      type="button"
      role="switch"
      class="switch"
      aria-checked={autostart}
      aria-labelledby="autostart-label"
      aria-describedby={autostartError ? 'autostart-error autostart-hint' : 'autostart-hint'}
      onclick={toggleAutostart}
    ><span></span></button>
  </div>

  <div class="switch-group" role="group" aria-labelledby="discord-presence-label">
    <div class="switch-row">
      <div>
        <div class="switch-label">
          <label for="discord-presence-switch" id="discord-presence-label">Show packraven in my Discord status</label>
          {@render badge('discord-presence')}
        </div>
        <p class="muted" id="discord-presence-hint">Share what you're doing in packraven while the window is open.</p>
        {#if discordPresenceError}<p class="failed" id="discord-presence-error" role="alert">{discordPresenceError}</p>{/if}
      </div>
      <button
        id="discord-presence-switch"
        type="button"
        role="switch"
        class="switch"
        aria-checked={settings.discordPresence}
        aria-labelledby="discord-presence-label"
        aria-describedby={discordPresenceError ? 'discord-presence-error discord-presence-hint' : 'discord-presence-hint'}
        onclick={toggleDiscordPresence}
      ><span></span></button>
    </div>

    <div class="switch-row sub-switch" class:inactive={!settings.discordPresence}>
      <div>
        <div class="switch-label">
          <label for="discord-shows-pack-switch" id="discord-shows-pack-label">Show the pack I'm working on</label>
          {@render badge('discord-shows-pack')}
        </div>
        <p class="muted" id="discord-shows-pack-hint">Turn this off to keep unannounced packs private. Discord still shows that you're using packraven, without the pack's name, icon, or file and mod names.</p>
        {#if discordShowsPackError}<p class="failed" id="discord-shows-pack-error" role="alert">{discordShowsPackError}</p>{/if}
      </div>
      <button
        id="discord-shows-pack-switch"
        type="button"
        role="switch"
        class="switch"
        aria-checked={settings.discordShowsPack}
        aria-labelledby="discord-shows-pack-label"
        aria-describedby={discordShowsPackError ? 'discord-shows-pack-error discord-shows-pack-hint' : 'discord-shows-pack-hint'}
        onclick={toggleDiscordShowsPack}
      ><span></span></button>
    </div>
  </div>

  <div class="switch-row" id="updates">
    <div class="updates">
      <div class="switch-label">
        <label for="auto-update-switch" id="auto-update-label">Keep packraven up to date automatically</label>
        {@render badge('auto-update')}
      </div>
      {#if autoUpdateError}<p class="failed" id="auto-update-error" role="alert">{autoUpdateError}</p>{/if}
      {#if selfUpdate.view}
        {@const view = selfUpdate.view}
        {@const installFailed = view.status.phase === 'failed' && view.status.stage === 'install'}
        <p class:muted={!installFailed} class:failed={installFailed} role="status">{updateStatusText(view)}</p>
        {#if view.status.phase === 'failed'}
          <details class="update-error">
            <summary>Details</summary>
            <p class="file">{view.status.error}</p>
          </details>
        {/if}
        {#if view.status.phase === 'downloading'}
          <progress max="100" value={view.status.percent ?? undefined} aria-labelledby="auto-update-label"></progress>
        {:else if view.supported}
          <div class="identity-row">
            {#if updateWaiting(view)}
              <button type="button" class="primary" disabled={!!view.blocked} aria-describedby={view.blocked ? 'update-blocked' : undefined} onclick={installNow}>
                {view.status.phase === 'available' ? 'Update and restart' : view.auto ? 'Restart now' : 'Restart to update'}
              </button>
              <button type="button" class="link" onclick={() => openUpdateChangelog(view.current, view.status.version, 'available')}>What's new</button>
            {:else}
              {@const busy = checkingUpdate || view.status.phase === 'checking'}
              <button type="button" class:busy disabled={busy} onclick={checkForUpdate}>Check for updates</button>
              <button type="button" class="link" onclick={() => openUpdateChangelog(selfUpdate.lastUpdatedFrom ?? view.current, view.current, 'current')}>What's new</button>
            {/if}
          </div>
          {#if updateWaiting(view) && view.blocked}<p class="muted hint" id="update-blocked">{view.blocked}</p>{/if}
        {/if}
      {/if}
    </div>
    <button
      id="auto-update-switch"
      type="button"
      role="switch"
      class="switch"
      aria-checked={settings.autoUpdate}
      aria-labelledby="auto-update-label"
      aria-describedby={autoUpdateError ? 'auto-update-error' : undefined}
      onclick={toggleAutoUpdate}
    ><span></span></button>
  </div>
  </div>

  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <form class="form" onsubmit={(e) => e.preventDefault()} onkeydown={onFormKey}>
    {#if saveError}<p class="failed" role="alert">{saveError}</p>{/if}

    <section aria-labelledby="machine-title">
      <h3 id="machine-title" tabindex="-1">This machine</h3>
      <div class="field">
        <div class="label-row">
          <label for="install-dir">CurseForge install folder</label>
          {@render badge('install-dir')}
        </div>
        <div class="path">
          <input
            id="install-dir"
            bind:value={settings.installDir}
            oninput={() => scheduleSave('install-dir', saveInstallDir)}
            onchange={() => flushField('install-dir', saveInstallDir)}
          />
          <button type="button" onclick={pickInstall}>Choose</button>
        </div>
      </div>
    </section>

    <section aria-labelledby="commits-title">
      <h3 id="commits-title" tabindex="-1">Commits</h3>
      <div class="field">
        <div class="label-row">
          <label for="git-name">Name</label>
          {@render badge('git-name')}
        </div>
        <input
          id="git-name"
          bind:value={settings.gitName}
          oninput={() => scheduleSave('git-name', saveGitName)}
          onchange={() => flushField('git-name', saveGitName)}
          placeholder="From your git config"
          autocomplete="name"
        />
      </div>
      <div class="field">
        <div class="label-row">
          <label for="git-email">Email</label>
          {@render badge('git-email')}
        </div>
        <input
          id="git-email"
          bind:value={settings.gitEmail}
          oninput={() => scheduleSave('git-email', saveGitEmail)}
          onchange={() => flushField('git-email', saveGitEmail)}
          placeholder="From your git config"
          autocomplete="email"
          aria-describedby="commit-identity-hint"
        />
      </div>
      <p class="muted hint" id="commit-identity-hint">
        packraven makes commits as this person when you publish or press commit. Leave a field empty to use your git config.
      </p>
      {#if credentials?.github.present}
        <div class="identity-row">
          <button type="button" class:busy={identityBusy} disabled={identityBusy} onclick={fillIdentityFromGithub}>Fill in from GitHub</button>
          {#if identityMessage}<span class:muted={identityMessage.ok} class:failed={!identityMessage.ok} role="status">{identityMessage.text}</span>{/if}
        </div>
      {/if}
    </section>

    <section aria-labelledby="editor-title">
      <h3 id="editor-title">Editor</h3>
      <div class="field">
        <div class="label-row">
          <label for="editor-ask-mb">Ask before opening files larger than (MB)</label>
          {@render badge('editor-ask')}
        </div>
        <input
          id="editor-ask-mb"
          type="number"
          min="1"
          step="1"
          bind:value={editorAskDraft}
          oninput={() => scheduleSave('editor-ask', saveEditorAsk)}
          onchange={() => flushField('editor-ask', saveEditorAsk)}
          aria-invalid={editorAskInvalid}
          aria-describedby={editorAskInvalid ? 'editor-ask-error editor-ask-hint' : 'editor-ask-hint'}
        />
      </div>
      {#if editorAskInvalid}
        <p class="hint failed" id="editor-ask-error">Enter a whole number of at least 1 MB.</p>
      {/if}
      <p class="muted hint" id="editor-ask-hint">Past this size, you can open a file in a simplified editor, without checking or folding, so typing stays responsive.</p>
      <div class="field">
        <div class="label-row">
          <label for="editor-max-mb">Never open files larger than (MB)</label>
          {@render badge('editor-max')}
        </div>
        <input
          id="editor-max-mb"
          type="number"
          min="1"
          step="1"
          bind:value={editorMaxDraft}
          oninput={() => scheduleSave('editor-max', saveEditorMax)}
          onchange={() => flushField('editor-max', saveEditorMax)}
          aria-invalid={editorMaxInvalid}
          aria-describedby={editorMaxInvalid ? 'editor-max-error editor-max-hint' : 'editor-max-hint'}
        />
      </div>
      {#if editorMaxInvalid}
        <p class="hint failed" id="editor-max-error">
          {editorMaxBelowAsk ? `Enter at least ${settings.editorAskMb} MB, so this stays at or above the first limit.` : 'Enter a whole number of at least 1 MB.'}
        </p>
      {/if}
      <p class="muted hint" id="editor-max-hint">Larger files can run the editor out of memory, so we offer your default app for them instead.</p>
    </section>
  </form>

  {#if defender?.active}
    <section class="removal" aria-labelledby="windows-security-title">
      <h3 id="windows-security-title" tabindex="-1">Windows Security</h3>
      {#if defender.missing.length}
        <p class="muted">
          Windows Defender can slow things down. Exclude {defender.missing.length === 1 ? 'this folder' : 'these folders'} to speed things up, but Defender will stop scanning files in {defender.missing.length === 1 ? 'it' : 'them'}.
          {defender.exclusionsReadable ? '' : "Windows only shows exclusions to administrators, so packraven may still list folders you excluded elsewhere."}
        </p>
        <ul class="folders">{#each defender.missing as path (path)}<li class="file">{path}</li>{/each}</ul>
        <div><button type="button" class:busy={defenderBusy} disabled={defenderBusy} onclick={excludeFolders}>{defenderBusy ? 'Waiting for Windows' : 'Exclude folders'}</button></div>
        <p class="muted">You'll get a pop-up from Windows.</p>
      {:else}
        <p class="muted">Windows Defender will skip the pack, CurseForge and packraven folders. Only install projects you trust.</p>
      {/if}
      <p class:muted={!defenderFailed} class:failed={defenderFailed} class:visually-hidden={!defenderMessage} role="status">{defenderMessage ?? ''}</p>
      <div class="check-line">
        <label class="check">
          <input type="checkbox" checked={!settings.defenderDismissed} onchange={(e) => setDefenderPrompt(e.currentTarget.checked, e.currentTarget)} />
          Remind me when these folders aren't excluded
        </label>
        {@render badge('defender-prompt')}
      </div>
    </section>
  {/if}
  {:else if appSettingsView.tab === 'java'}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <form class="form" onsubmit={(e) => e.preventDefault()} onkeydown={onFormKey}>
    {#if saveError}<p class="failed" role="alert">{saveError}</p>{/if}

    <section aria-labelledby="java-title">
      <h3 id="java-title" tabindex="-1">Java</h3>
      <p class="muted">
        On Automatic, we use CurseForge's Java for each version, or another Java of that version on this computer. A newer Java runs most packs from
        Minecraft 1.18 on, though some mods may break. To use a different Java for one pack, choose it in Pack settings.
      </p>
      {#each JAVA_SLOTS as slot (slot.major)}
        <JavaPicker
          id="java-{slot.major}"
          label={slot.versions}
          major={Number(slot.major)}
          bind:value={() => settings!.javaByMajor[slot.major] ?? '', (path) => (settings!.javaByMajor[slot.major] = path)}
          defaultName="Automatic"
          defaultPath={javas.scan?.automatic[slot.major] ?? null}
          needed={javas.scan?.needed.includes(Number(slot.major)) ?? false}
          onchange={() => saveJavaSlot(slot.major)}
          ondownload={reloadJavaPaths}
        >
          {#snippet status()}{@render badge(`java-${slot.major}`)}{/snippet}
        </JavaPicker>
      {/each}
      <p class="muted hint java-scan" role="status">
        {#if javas.error}
          <span class="failed">Couldn't look for Java: {javas.error}</span>
        {:else if javas.scan}
          {javas.scan.installs.length === 1 ? '1 Java install' : `${javas.scan.installs.length} Java installs`} found on this computer.
        {/if}
        <button type="button" class="link" disabled={javas.scanning} onclick={scanJava}>{javas.scanning ? 'Looking' : 'Look again'}</button>
      </p>
      <ExtraLaunchArgs
        bind:draft={runArgs}
        scope="app"
        onedit={() => saveState !== 'failed' && (saveState = 'idle')}
        onchange={saveRunArgs}
      >
        {#snippet status(key)}{@render badge(key)}{/snippet}
      </ExtraLaunchArgs>
    </section>
  </form>
  {/if}

  {#if appSettingsView.tab === 'accounts'}
  {#if credentialsError}
    <section class="removal" aria-labelledby="access-tokens-title">
      <h3 id="access-tokens-title">Access tokens</h3>
      <Notice tone="error">
        <p>Couldn't read your saved CurseForge, Modrinth and GitHub tokens: {credentialsError}</p>
        <p><button type="button" onclick={loadCredentials}>Try again</button></p>
      </Notice>
    </section>
  {:else if credentials}
    <section class="removal" id="curseforge-access" aria-labelledby="curseforge-access-title">
      <h3 id="curseforge-access-title" tabindex="-1">CurseForge access</h3>
      <p class="muted">Changes here take effect right away, including in the git hooks.</p>
      {#if !credentials.builtInApiKey}
        <Notice tone="warn">
          <p>This packraven build has no CurseForge API key, so we can't look up mods or keep jars you added by hand out of git. To fix this, build packraven with <code>PACKRAVEN_CF_API_KEY</code> set.</p>
        </Notice>
      {/if}
      <CredentialField credential="upload" label="CurseForge upload token" status={credentials.upload} keychain={credentials.keychain} {envNames} onchange={loadCredentials}>
        {#snippet help()}
          <p class="muted hint">
            You only need this to publish to CurseForge. <a href={UPLOAD_TOKEN_PAGE} onclick={(e) => { e.preventDefault(); openUrl(UPLOAD_TOKEN_PAGE) }}>Generate a token on CurseForge</a>, then paste it here.
          </p>
        {/snippet}
      </CredentialField>
    </section>

    <section class="removal" id="modrinth-access" aria-labelledby="modrinth-access-title">
      <h3 id="modrinth-access-title" tabindex="-1">Modrinth access</h3>
      <CredentialField credential="modrinth" label="Modrinth token" status={credentials.modrinth} keychain={credentials.keychain} {envNames} onchange={loadCredentials}>
        {#snippet help()}
          <p class="muted hint">
            You only need this to publish to Modrinth. <a href={MODRINTH_TOKEN_PAGE} onclick={(e) => { e.preventDefault(); openUrl(MODRINTH_TOKEN_PAGE) }}>Create a personal access token</a>
            with the Create versions scope, then paste it here.
          </p>
        {/snippet}
      </CredentialField>
    </section>

    <section class="removal" id="github-access" aria-labelledby="github-access-title">
      <h3 id="github-access-title" tabindex="-1">GitHub access</h3>
      <CredentialField credential="github" label="GitHub token" status={credentials.github} keychain={credentials.keychain} {envNames} onchange={loadCredentials}>
        {#snippet help()}
          <p class="muted hint">
            packraven uses this to pull and push when your pack's remote is an HTTPS URL on github.com. Other remotes keep using your git setup.
            <a href={GITHUB_TOKEN_PAGE} onclick={(e) => { e.preventDefault(); openUrl(GITHUB_TOKEN_PAGE) }}>Create a fine-grained token</a> for the pack's
            repository with Contents: Read and write, then paste it here.
          </p>
        {/snippet}
      </CredentialField>
    </section>
  {/if}
  {/if}
  </div>
  </div>
{/if}

<style>
  .identity-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 12px;
  }

  .settings {
    display: grid;
    gap: 20px;
    max-width: 760px;
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

  .panel {
    display: grid;
    gap: 36px;
  }

  .folders {
    margin: 0;
    padding-left: 18px;
    overflow-wrap: anywhere;
  }

  .removal {
    padding-top: 28px;
    border-top: 1px solid var(--line);
  }

  header p {
    margin-top: 4px;
  }

  .account {
    display: grid;
    gap: 12px;
  }

  .account h3,
  .account p {
    margin: 0;
  }

  .app-switches {
    display: grid;
    gap: 24px;
    padding-bottom: 28px;
    border-bottom: 1px solid var(--line);
  }

  .switch-group {
    display: grid;
    gap: 14px;
  }

  .switch-row {
    display: flex;
    justify-content: space-between;
    align-items: start;
    gap: 24px;
  }

  .sub-switch {
    margin-left: 16px;
    padding-left: 16px;
    border-left: 2px solid var(--line-strong);
  }

  .sub-switch.inactive {
    opacity: 0.72;
  }

  .switch-label {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 10px;
  }

  .switch-label label {
    color: var(--text);
    font-size: var(--step-0);
    cursor: pointer;
  }

  .updates {
    display: grid;
    gap: 8px;
  }

  .updates p {
    margin: 0;
  }

  .updates .hint {
    margin-top: 0;
  }

  .updates progress {
    width: min(320px, 100%);
    accent-color: var(--forest);
  }

  .update-error summary {
    width: fit-content;
    color: var(--muted);
    font-size: var(--step--1);
    cursor: pointer;
  }

  .update-error p {
    margin-top: 4px;
    font-size: var(--step--1);
    overflow-wrap: anywhere;
  }

  .switch {
    position: relative;
    flex: none;
    width: 46px;
    height: 26px;
    padding: 0;
    border-radius: 13px;
    background: var(--control);
    border-color: var(--edge);
  }

  .switch span {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--muted);
    transition: transform 0.15s ease;
  }

  .switch[aria-checked='true'] {
    background: var(--forest);
    border-color: var(--forest);
  }

  .switch[aria-checked='true']:hover {
    background: var(--forest-hi);
    border-color: var(--forest-hi);
  }

  .switch[aria-checked='true'] span {
    background: var(--paper);
    transform: translateX(20px);
  }

  @media (prefers-reduced-motion: reduce) {
    .switch span {
      transition: none;
    }
  }

  .hint {
    color: var(--muted);
    font-size: var(--step--1);
  }

  .field + .hint {
    margin-top: -6px;
  }

  .hint.failed {
    color: var(--fail);
  }

  .form {
    display: grid;
    gap: 36px;
  }

  section {
    display: grid;
    gap: 14px;
  }

  .field {
    display: grid;
    gap: 6px;
  }

  .label-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    color: var(--text);
    font-size: var(--step-0);
  }

  .label-row > label {
    color: var(--text);
    font-size: var(--step-0);
  }

  .check-line {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 10px;
  }

  .path {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .path input {
    flex: 1;
  }
</style>
