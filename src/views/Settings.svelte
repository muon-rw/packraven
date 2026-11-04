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
  import { api, errorText, type CredentialsView, type DefenderView, type RunArgs, type Settings, type UpdateView } from '../lib/api'
  import { installNow, openUpdateChangelog, selfUpdate, updateWaiting } from '../lib/selfUpdate.svelte'
  import { javas, scanJava } from '../lib/java.svelte'
  import { trackSave } from '../lib/launch.svelte'
  import { cancelSignIn, minecraft, signIn, signOut } from '../lib/minecraft.svelte'

  let { onchange }: { onchange: () => void } = $props()

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

  let account = $derived(minecraft.account)
  let signingIn = $derived(minecraft.signingIn)
  let accountError = $state<{ text: string; retry: () => void } | null>(null)
  // We keep one live region and change its text, since screen readers often miss a status that appears.
  let accountStatus = $state('')
  let accountButton: HTMLButtonElement | undefined = $state()

  // Each state swaps the button you just pressed for another, so we move focus to the new one.
  async function focusAccountButton() {
    await tick()
    accountButton?.focus()
  }

  async function loadAccount() {
    try {
      minecraft.account = await api.minecraftAccount()
      accountError = null
    } catch (error) {
      accountError = { text: `Couldn't read your Minecraft sign-in: ${errorText(error)}`, retry: loadAccount }
    }
  }

  async function startSignIn() {
    accountError = null
    accountStatus = 'Waiting for you to sign in with Microsoft in your browser.'
    const signingIn = signIn()
    focusAccountButton()
    try {
      const signedIn = await signingIn
      accountStatus = signedIn ? `Signed in as ${signedIn.name}.` : 'Sign-in cancelled.'
    } catch (error) {
      accountStatus = ''
      accountError = { text: errorText(error), retry: startSignIn }
    } finally {
      focusAccountButton()
    }
  }

  async function startSignOut() {
    accountError = null
    try {
      await signOut()
      accountStatus = 'Signed out of Minecraft.'
      focusAccountButton()
    } catch (error) {
      accountError = { text: errorText(error), retry: startSignOut }
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

  onDestroy(() => {
    for (const { timer, commit } of timers.values()) {
      clearTimeout(timer)
      commit()
    }
    timers.clear()
  })

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

  function saveJavaSlotQuiet(major: string, path: string) {
    if (!settings || !savedSettings) return
    const next = path.trim()
    settings.javaByMajor[major] = next
    savedSettings.javaByMajor[major] = next
    saving = trackSave(
      saving.then(async () => {
        try {
          const fresh = await api.getSettings()
          if (next) fresh.javaByMajor[major] = next
          else delete fresh.javaByMajor[major]
          fresh.javaByMajor = cleanJavaByMajor(fresh.javaByMajor)
          await api.saveSettings(fresh)
          onchange()
        } catch (error) {
          saveError = `Couldn't save: ${errorText(error)}`
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

  async function toggleAutoUpdate() {
    if (!settings) return
    const wanted = !settings.autoUpdate
    settings.autoUpdate = wanted
    savedField = 'auto-update'
    saveState = 'saving'
    try {
      autoUpdateError = null
      await api.setAutoUpdate(wanted)
      if (savedSettings) savedSettings.autoUpdate = wanted
      if (savedField === 'auto-update') {
        saveTick += 1
        saveState = 'saved'
      }
    } catch (error) {
      settings.autoUpdate = !wanted
      if (savedField === 'auto-update') saveState = 'failed'
      autoUpdateError = `Couldn't change the update setting: ${errorText(error)}`
    }
  }

  let discordShowsPackError: string | null = $state(null)

  async function toggleDiscordShowsPack() {
    if (!settings) return
    const wanted = !settings.discordShowsPack
    settings.discordShowsPack = wanted
    savedField = 'discord-shows-pack'
    saveState = 'saving'
    try {
      discordShowsPackError = null
      await api.setDiscordShowsPack(wanted)
      if (savedSettings) savedSettings.discordShowsPack = wanted
      if (savedField === 'discord-shows-pack') {
        saveTick += 1
        saveState = 'saved'
      }
    } catch (error) {
      settings.discordShowsPack = !wanted
      if (savedField === 'discord-shows-pack') saveState = 'failed'
      discordShowsPackError = `Couldn't change the Discord setting: ${errorText(error)}`
    }
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

  <section class="account" id="minecraft-account" aria-labelledby="minecraft-account-title">
    <h3 id="minecraft-account-title">Minecraft account</h3>
    {#if account}
      <p>
        Signed in as <strong>{account.name}</strong>. Test clients on this computer launch as this account.
      </p>
    {:else if signingIn}
      <p class="muted">Finish signing in with Microsoft in your browser. packraven is waiting.</p>
    {:else}
      <p class="muted">
        Mojang only lets launchers start the game for an account that owns it, so sign in with a Microsoft account that owns Minecraft: Java Edition to
        launch test clients. You can start test servers without signing in.
      </p>
    {/if}
    <div class="identity-row">
      {#if account}
        <button type="button" bind:this={accountButton} onclick={startSignOut}>Sign out</button>
      {:else if signingIn}
        <button type="button" bind:this={accountButton} onclick={cancelSignIn}>Cancel</button>
      {:else}
        <button type="button" class="primary" bind:this={accountButton} onclick={startSignIn}>Sign in with Microsoft</button>
      {/if}
    </div>
    <p class="visually-hidden" role="status">{accountStatus}</p>
    {#if accountError}
      <Notice tone="error">
        <p>{accountError.text}</p>
        <p><button type="button" onclick={accountError.retry}>Try again</button></p>
      </Notice>
    {/if}
    <p class="muted hint">packraven is not an official Minecraft product, and is not approved by or associated with Mojang or Microsoft.</p>
  </section>

  <div class="app-switches">
  <div class="switch-row">
    <div>
      <div class="switch-label">
        <span id="autostart-label">Start in the tray when I sign in to this computer</span>
        {@render badge('autostart')}
      </div>
      <p class="muted" id="autostart-hint">Keep packraven running so builds and test servers are a click away.</p>
      {#if autostartError}<p class="failed" role="alert">{autostartError}</p>{/if}
    </div>
    <button
      type="button"
      role="switch"
      class="switch"
      aria-checked={autostart}
      aria-labelledby="autostart-label"
      aria-describedby="autostart-hint"
      onclick={toggleAutostart}
    ><span></span></button>
  </div>

  <div class="switch-row">
    <div>
      <div class="switch-label">
        <span id="discord-shows-pack-label">Show the pack I'm working on in my Discord status</span>
        {@render badge('discord-shows-pack')}
      </div>
      <p class="muted" id="discord-shows-pack-hint">Turn this off to keep unannounced packs private. Discord still shows that you're using packraven.</p>
      {#if discordShowsPackError}<p class="failed" role="alert">{discordShowsPackError}</p>{/if}
    </div>
    <button
      type="button"
      role="switch"
      class="switch"
      aria-checked={settings.discordShowsPack}
      aria-labelledby="discord-shows-pack-label"
      aria-describedby="discord-shows-pack-hint"
      onclick={toggleDiscordShowsPack}
    ><span></span></button>
  </div>

  <div class="switch-row" id="updates">
    <div class="updates">
      <div class="switch-label">
        <span id="auto-update-label">Keep packraven up to date automatically</span>
        {@render badge('auto-update')}
      </div>
      {#if autoUpdateError}<p class="failed" role="alert">{autoUpdateError}</p>{/if}
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
      type="button"
      role="switch"
      class="switch"
      aria-checked={settings.autoUpdate}
      aria-labelledby="auto-update-label"
      onclick={toggleAutoUpdate}
    ><span></span></button>
  </div>
  </div>

  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <form class="form" onsubmit={(e) => e.preventDefault()} onkeydown={onFormKey}>
    {#if saveError}<p class="failed" role="alert">{saveError}</p>{/if}

    <section aria-labelledby="machine-title">
      <h3 id="machine-title">This machine</h3>
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
      <h3 id="commits-title">Commits</h3>
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

    <section aria-labelledby="java-title">
      <h3 id="java-title">Java</h3>
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
          ondownload={(java) => {
            // The backend only chooses a download for a slot when Automatic wouldn't pick it, like GraalVM 21 for Java 17.
            if (java.major !== Number(slot.major)) saveJavaSlotQuiet(slot.major, java.path)
          }}
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
      <h3 id="github-access-title">GitHub access</h3>
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

  {#if defender?.active}
    <section class="removal" aria-labelledby="windows-security-title">
      <h3 id="windows-security-title">Windows Security</h3>
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
    gap: 36px;
    max-width: 760px;
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
    padding-bottom: 28px;
    border-bottom: 1px solid var(--line);
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

  .switch-row {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 24px;
  }

  .switch-label {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 10px;
  }

  /* The status and buttons below the label would otherwise pull the switch down, away from it. */
  #updates {
    align-items: start;
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
