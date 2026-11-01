<script lang="ts">
  import Notice from '../components/Notice.svelte'
  import JavaPicker from '../components/JavaPicker.svelte'
  import ExtraLaunchArgs, { activeLines, emptyDraft, fromDraft, toDraft } from '../components/ExtraLaunchArgs.svelte'
  import SaveBadge from '../components/SaveBadge.svelte'
  import { onDestroy, onMount } from 'svelte'
  import { ask } from '@tauri-apps/plugin-dialog'
  import { api, errorText, type PackConfig, type PackJavaDefault, type RunArgs } from '../lib/api'
  import OverridePermissions from '../components/OverridePermissions.svelte'
  import { scanJava } from '../lib/java.svelte'
  import { trackSave } from '../lib/launch.svelte'
  import { serverView } from '../lib/nav.svelte'

  let {
    dir,
    name,
    onserver,
    onappsettings,
    onchange,
    onremoved,
  }: { dir: string; name: string; onserver: () => void; onappsettings: () => void; onchange: () => void; onremoved: () => void } = $props()

  let config: PackConfig | null = $state(null)
  let savedConfig: PackConfig | null = $state(null)
  let configError: string | null = $state(null)
  let cfProjectDraft = $state<number | null>(null)
  let modrinthProjectDraft = $state('')
  let portDraft = $state<number | null>(25565)
  let clientMemoryDraft = $state('8G')
  let webhook = $state('')
  let savedWebhook = $state('')
  let packJava = $state('')
  let savedPackJava = $state('')
  let runArgs = $state(emptyDraft())
  let savedRunArgs = $state<RunArgs>(fromDraft(emptyDraft()))
  let appRunArgs = $state(0)
  let javaDefault = $state<PackJavaDefault | null>(null)
  let savedField = $state<string | null>(null)
  let saveState = $state<'idle' | 'saving' | 'saved' | 'failed'>('idle')
  let saveTick = $state(0)
  let saveError = $state<string | null>(null)
  let discordMessage = $state<string | null>(null)
  let discordFailed = $state(false)
  let testing = $state(false)
  let removeError: string | null = $state(null)

  const validMemory = (text: string) => /^\d+[mg]$/i.test(text.trim())
  let cfProjectInvalid = $derived(cfProjectDraft !== null && (!Number.isInteger(cfProjectDraft) || cfProjectDraft < 0))
  let portInvalid = $derived(portDraft === null || !Number.isInteger(portDraft) || portDraft < 1 || portDraft > 65535)
  let clientMemoryInvalid = $derived(!validMemory(clientMemoryDraft))

  async function load() {
    try {
      const loadedArgs = await api.getRunArgs()
      runArgs = toDraft(loadedArgs)
      savedRunArgs = fromDraft(runArgs)
      // Only the App settings hint depends on this, so a failed read shouldn't hide the pack's settings.
      appRunArgs = await api.getSettings().then((settings) => activeLines(settings.runArgs), () => 0)
      const loadedConfig = await api.getPackConfig()
      config = loadedConfig
      savedConfig = structuredClone(loadedConfig)
      cfProjectDraft = loadedConfig.curseforge.project_id || null
      modrinthProjectDraft = loadedConfig.modrinth.project_id
      portDraft = loadedConfig.test.port
      clientMemoryDraft = loadedConfig.test.client_memory
      checkModrinthProject(loadedConfig.modrinth.project_id.trim())
      webhook = (await api.getWebhook()) ?? ''
      savedWebhook = webhook.trim()
      packJava = (await api.getPackJava()) ?? ''
      savedPackJava = packJava.trim()
      configError = null
      await loadJavaDefault()
    } catch (error) {
      config = null
      savedConfig = null
      configError = errorText(error)
    }
  }

  async function loadJavaDefault() {
    javaDefault = await api.packJavaDefault().catch(() => null)
  }

  onMount(() => {
    scanJava()
    load()
  })

  // Other views and permissions sections write packraven.toml on their own, so each save merges only
  // the changed field into a fresh read and chains in order.
  let saving = Promise.resolve()

  function runSave(field: string, action: () => Promise<void>) {
    savedField = field
    saveState = 'saving'
    saveError = null
    saving = trackSave(
      saving.then(async () => {
        try {
          await action()
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

  function saveConfigField(field: string, apply: (target: PackConfig) => void) {
    if (!config || !savedConfig) return
    apply(config)
    apply(savedConfig)
    runSave(field, async () => {
      const fresh = await api.getPackConfig()
      apply(fresh)
      await api.savePackConfig(fresh)
    })
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

  function saveCfProject() {
    if (!savedConfig || cfProjectInvalid) return
    const next = cfProjectDraft ?? 0
    if (next === savedConfig.curseforge.project_id) return
    saveConfigField('curseforge-project', (c) => (c.curseforge.project_id = next))
  }

  function saveModrinthProject() {
    if (!config || !savedConfig) return
    const next = modrinthProjectDraft.trim()
    if (next === savedConfig.modrinth.project_id.trim()) return
    saveConfigField('modrinth-project', (c) => (c.modrinth.project_id = next))
    checkModrinthProject(next)
  }

  function saveReleaseType() {
    if (!config || !savedConfig || config.curseforge.release_type === savedConfig.curseforge.release_type) return
    const next = config.curseforge.release_type
    saveConfigField('release-type', (c) => (c.curseforge.release_type = next))
  }

  function saveAuthor() {
    if (!config || !savedConfig || config.author === savedConfig.author) return
    const next = config.author
    saveConfigField('author', (c) => (c.author = next))
  }

  function saveManualRelease() {
    if (!config || !savedConfig || config.curseforge.manual_release === savedConfig.curseforge.manual_release) return
    const next = config.curseforge.manual_release
    saveConfigField('manual-release', (c) => (c.curseforge.manual_release = next))
  }

  function savePort() {
    if (!savedConfig || portInvalid || portDraft === null || portDraft === savedConfig.test.port) return
    const next = portDraft
    saveConfigField('port', (c) => (c.test.port = next))
  }

  function saveClientMemory() {
    if (!savedConfig || clientMemoryInvalid) return
    const next = clientMemoryDraft.trim()
    if (next === savedConfig.test.client_memory) return
    saveConfigField('client-memory', (c) => (c.test.client_memory = next))
  }

  function savePackJava() {
    const next = packJava.trim()
    if (next === savedPackJava) return
    savedPackJava = next
    runSave('pack-java', async () => {
      await api.setPackJava(next || null)
      await loadJavaDefault()
    })
  }

  function saveRunArgs(key: keyof RunArgs) {
    const next = fromDraft(runArgs)
    if (JSON.stringify(next) === JSON.stringify(savedRunArgs)) return
    savedRunArgs = next
    runSave(key, () => api.setRunArgs(next))
  }

  function saveWebhook() {
    const next = webhook.trim()
    if (next === savedWebhook) return
    savedWebhook = next
    runSave('webhook', () => api.setWebhook(webhook))
  }

  function saveDiscordField<K extends keyof PackConfig['discord']>(field: string, key: K) {
    if (!config || !savedConfig || config.discord[key] === savedConfig.discord[key]) return
    const next = config.discord[key]
    saveConfigField(field, (c) => (c.discord[key] = next))
  }

  async function testWebhook() {
    testing = true
    clearEdit('webhook')
    try {
      savedWebhook = webhook.trim()
      await api.setWebhook(webhook)
      await api.discordTest()
      discordFailed = false
      discordMessage = 'Sent. Check the channel.'
    } catch (error) {
      discordFailed = true
      discordMessage = errorText(error)
    } finally {
      testing = false
    }
  }

  let modrinthProject = $state<{ id: string; title: string | null } | null>(null)
  let modrinthMissing = $derived(!!modrinthProject && modrinthProject.id === modrinthProjectDraft.trim() && modrinthProject.title === null)

  // A mistyped project would otherwise only show up when you publish.
  async function checkModrinthProject(id: string) {
    if (!id) return (modrinthProject = null)
    try {
      const title = await api.modrinthProjectTitle(id)
      if (modrinthProjectDraft.trim() === id) modrinthProject = { id, title }
    } catch {
      if (modrinthProjectDraft.trim() === id) modrinthProject = null
    }
  }

  async function removePack() {
    const confirmed = await ask(`Remove ${name} from packraven? The instance folder, its files and its git repository stay as they are.`, {
      title: 'Remove pack',
      kind: 'warning',
      okLabel: 'Remove pack',
      cancelLabel: 'Cancel',
    })
    if (!confirmed) return
    try {
      removeError = null
      await api.removePack(dir)
      onremoved()
    } catch (error) {
      removeError = `Couldn't remove the pack: ${errorText(error)}`
    }
  }
</script>

{#snippet badge(field: string)}
  <SaveBadge active={savedField === field} state={saveState} tick={saveTick} />
{/snippet}

<div class="settings">
  <header>
    <h2>Pack settings</h2>
    <p class="muted">For {name} only. packraven stores these in <span class="file">packraven.toml</span> in the pack repo, except the Java, the extra launch arguments and the webhook.</p>
  </header>

  {#if configError}
    <Notice tone="error">
      <p>Couldn't read packraven.toml: {configError}</p>
      <p><button onclick={load}>Try again</button></p>
    </Notice>
  {:else if config}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <form class="form" onsubmit={(e) => e.preventDefault()} onkeydown={onFormKey}>
      {#if saveError}<p class="failed" role="alert">{saveError}</p>{/if}

      <section aria-labelledby="publishing-title">
        <h3 id="publishing-title" tabindex="-1">Publishing</h3>
        <div class="grid">
          <div class="field">
            <div class="label-row">
              <label for="cf-project-id">CurseForge project ID</label>
              {@render badge('curseforge-project')}
            </div>
            <input
              id="cf-project-id"
              type="number"
              min="0"
              step="1"
              bind:value={cfProjectDraft}
              oninput={() => scheduleSave('curseforge-project', saveCfProject)}
              onchange={() => flushField('curseforge-project', saveCfProject)}
              placeholder="Not set"
              aria-invalid={cfProjectInvalid}
              aria-describedby={cfProjectInvalid ? 'cf-project-hint' : undefined}
            />
            {#if cfProjectInvalid}
              <span class="hint failed" id="cf-project-hint">Enter a whole number, or leave it empty.</span>
            {/if}
          </div>
          <div class="field">
            <div class="label-row">
              <label for="modrinth-project">Modrinth project</label>
              {@render badge('modrinth-project')}
            </div>
            <input
              id="modrinth-project"
              bind:value={modrinthProjectDraft}
              oninput={() => scheduleSave('modrinth-project', saveModrinthProject)}
              onchange={() => flushField('modrinth-project', saveModrinthProject)}
              placeholder="Not set"
              spellcheck="false"
              aria-invalid={modrinthMissing}
              aria-describedby="modrinth-project-hint"
            />
            <span class="hint" id="modrinth-project-hint">
              {#if modrinthProject && modrinthProject.id === modrinthProjectDraft.trim()}
                {#if modrinthProject.title}On Modrinth as {modrinthProject.title}{:else}<span class="failed">Modrinth has no project with this ID or slug</span>{/if}
              {:else}
                The ID, or the slug from the project's URL
              {/if}
            </span>
          </div>
          <div class="field">
            <div class="label-row">
              <label for="release-type">Release type</label>
              {@render badge('release-type')}
            </div>
            <select id="release-type" bind:value={config.curseforge.release_type} onchange={saveReleaseType} aria-describedby="release-type-hint">
              <option value="release">Release</option>
              <option value="beta">Beta</option>
              <option value="alpha">Alpha</option>
            </select>
            <span class="hint" id="release-type-hint">On both platforms</span>
          </div>
          <div class="field">
            <div class="label-row">
              <label for="pack-author">Author</label>
              {@render badge('author')}
            </div>
            <input
              id="pack-author"
              bind:value={config.author}
              oninput={() => scheduleSave('author', saveAuthor)}
              onchange={() => flushField('author', saveAuthor)}
            />
          </div>
        </div>
        <p class="hint">packraven publishes to each platform you give a project.</p>
        <div class="check-line">
          <label class="check">
            <input type="checkbox" bind:checked={config.curseforge.manual_release} onchange={saveManualRelease} /> Hold approved files until I release them on CurseForge
          </label>
          {@render badge('manual-release')}
        </div>
      </section>

      <section aria-labelledby="test-runs-title">
        <h3 id="test-runs-title">Test runs</h3>
        <div class="grid">
          <div class="field">
            <div class="label-row">
              <label for="test-port">Server port</label>
              {@render badge('port')}
            </div>
            <input
              id="test-port"
              type="number"
              min="1"
              max="65535"
              step="1"
              bind:value={portDraft}
              oninput={() => scheduleSave('port', savePort)}
              onchange={() => flushField('port', savePort)}
              aria-invalid={portInvalid}
              aria-describedby={portInvalid ? 'test-port-hint test-memory-hint' : 'test-memory-hint'}
            />
            {#if portInvalid}
              <span class="hint failed" id="test-port-hint">Enter a port between 1 and 65535, such as 25565.</span>
            {/if}
          </div>
          <div class="field">
            <div class="label-row">
              <label for="client-memory">Client memory</label>
              {@render badge('client-memory')}
            </div>
            <input
              id="client-memory"
              bind:value={clientMemoryDraft}
              oninput={() => scheduleSave('client-memory', saveClientMemory)}
              onchange={() => flushField('client-memory', saveClientMemory)}
              placeholder="8G"
              spellcheck="false"
              aria-invalid={clientMemoryInvalid}
              aria-describedby={clientMemoryInvalid ? 'client-memory-hint test-memory-hint' : 'test-memory-hint'}
            />
            {#if clientMemoryInvalid}
              <span class="hint failed" id="client-memory-hint">Enter a size in gigabytes or megabytes, such as 8G or 8192M.</span>
            {/if}
          </div>
        </div>
        <p class="hint" id="test-memory-hint">
          These are for testing launches only. The memory setting is for the Client only, you can type values like 8G or 8192M. For server pack memory, Java and flags, open <button type="button" class="link" onclick={() => ((serverView.tab = 'startup'), onserver())}>Server, Startup</button>.
        </p>
        {#if javaDefault}
          <JavaPicker
            id="pack-java"
            label="Java on this machine (this pack needs Java {javaDefault.major})"
            major={javaDefault.major}
            bind:value={packJava}
            defaultName="Same as App settings"
            defaultPath={javaDefault.path}
            needed
            hint="pack-java-hint"
            onchange={savePackJava}
            ondownload={loadJavaDefault}
          >
            {#snippet status()}{@render badge('pack-java')}{/snippet}
          </JavaPicker>
          <p class="hint" id="pack-java-hint">
            packraven uses this Java for the pack's test runs and server builds, and saves your choice only on this machine, never in the pack repo. To choose a Java for every pack, open <button type="button" class="link" onclick={onappsettings}>App settings</button>.
          </p>
        {/if}
        <ExtraLaunchArgs
          bind:draft={runArgs}
          scope="pack"
          inherited={appRunArgs}
          {onappsettings}
          onedit={() => saveState !== 'failed' && (saveState = 'idle')}
          onchange={saveRunArgs}
        >
          {#snippet status(key)}{@render badge(key)}{/snippet}
        </ExtraLaunchArgs>
      </section>

      <section aria-labelledby="discord-title">
        <h3 id="discord-title">Discord announcements</h3>
        <div class="field">
          <div class="label-row">
            <label for="webhook">Webhook URL</label>
            {@render badge('webhook')}
          </div>
          <div class="inline">
            <input
              id="webhook"
              type="password"
              autocomplete="off"
              bind:value={webhook}
              oninput={() => {
                discordMessage = null
                scheduleSave('webhook', saveWebhook)
              }}
              onchange={() => flushField('webhook', saveWebhook)}
              placeholder="https://discord.com/api/webhooks/..."
              aria-describedby="webhook-hint"
            />
            <button type="button" class:busy={testing} disabled={testing || !webhook.trim()} onclick={testWebhook}>{testing ? 'Sending' : 'Send test message'}</button>
          </div>
          <p class="hint" id="webhook-hint">
            Copy it from the channel's Settings, Integrations, Webhooks. packraven saves it only on this machine, never in the pack repo.
          </p>
          {#if discordMessage}<p class:muted={!discordFailed} class:failed={discordFailed} role="status">{discordMessage}</p>{/if}
        </div>
        <div class="grid">
          <div class="field">
            <div class="label-row">
              <label for="discord-role">Role to ping</label>
              {@render badge('role')}
            </div>
            <input
              id="discord-role"
              bind:value={config.discord.role_id}
              oninput={() => scheduleSave('role', () => saveDiscordField('role', 'role_id'))}
              onchange={() => flushField('role', () => saveDiscordField('role', 'role_id'))}
              placeholder="Role ID"
              aria-describedby="role-hint"
            />
          </div>
          <div class="field">
            <div class="label-row">
              <label for="discord-color">Embed color</label>
              {@render badge('color')}
            </div>
            <input id="discord-color" type="color" bind:value={config.discord.color} onchange={() => saveDiscordField('color', 'color')} />
          </div>
        </div>
        <p class="hint" id="role-hint">
          To get a role ID, turn on Developer Mode in Discord's Advanced settings, then right-click the role and choose Copy Role ID. Leave it empty
          for no ping.
        </p>
        <div class="block">
          <div class="label-row">
            <label for="discord-title-input">Title</label>
            {@render badge('title')}
          </div>
          <input
            id="discord-title-input"
            bind:value={config.discord.title}
            oninput={() => scheduleSave('title', () => saveDiscordField('title', 'title'))}
            onchange={() => flushField('title', () => saveDiscordField('title', 'title'))}
            placeholder={'{project_name} - v{version}'}
            aria-describedby="discord-placeholders"
          />
        </div>
        <div class="block">
          <div class="label-row">
            <label for="discord-header">Text above the changelog</label>
            {@render badge('header')}
          </div>
          <textarea
            id="discord-header"
            rows="3"
            bind:value={config.discord.header}
            oninput={() => scheduleSave('header', () => saveDiscordField('header', 'header'))}
            onchange={() => flushField('header', () => saveDiscordField('header', 'header'))}
            aria-describedby="discord-placeholders"
          ></textarea>
        </div>
        <div class="block">
          <div class="label-row">
            <label for="discord-footer">Text below the changelog</label>
            {@render badge('footer')}
          </div>
          <textarea
            id="discord-footer"
            rows="4"
            bind:value={config.discord.footer}
            oninput={() => scheduleSave('footer', () => saveDiscordField('footer', 'footer'))}
            onchange={() => flushField('footer', () => saveDiscordField('footer', 'footer'))}
            placeholder={'[Install]({file_url})\n**Back up your world before updating**'}
            aria-describedby="discord-placeholders"
          ></textarea>
        </div>
        <p class="hint" id="discord-placeholders">
          These use Discord markdown and appear in every announcement. <code>{'{file_url}'}</code> links to the released file, <code>{'{project_url}'}</code> to the project page. <code>{'{project_name}'}</code> is the CurseForge project's name, <code>{'{name}'}</code> the instance name, and <code>{'{version}'}</code> the version. packraven uses the project logo as the thumbnail.
        </p>
        <div class="check-line">
          <label class="check">
            <input type="checkbox" bind:checked={config.discord.show_author} onchange={() => saveDiscordField('show-author', 'show_author')} /> Show the project author, with their CurseForge avatar and a link to their profile
          </label>
          {@render badge('show-author')}
        </div>
        <div class="check-line">
          <label class="check">
            <input type="checkbox" bind:checked={config.discord.announce_on_approval} onchange={() => saveDiscordField('announce-on-approval', 'announce_on_approval')} /> Announce each release once every platform it's on has approved it
          </label>
          {@render badge('announce-on-approval')}
        </div>
        <p class="hint">packraven checks every few minutes while it's running in the tray. You can also announce a release by hand from Releases.</p>
      </section>
    </form>

    <OverridePermissions platform="curseforge" targeted={config.curseforge.project_id > 0} reloadKey={config.curseforge.project_id} {onchange} />
    <OverridePermissions platform="modrinth" targeted={!!config.modrinth.project_id.trim()} reloadKey={config.modrinth.project_id} {onchange} />
  {/if}

  <section class="removal" aria-labelledby="remove-pack-title">
    <h3 id="remove-pack-title">Remove pack</h3>
    <p class="muted">Stop showing <span class="file path">{dir}</span> in packraven, without deleting anything on disk.</p>
    <div><button type="button" class="danger" onclick={removePack}>Remove pack</button></div>
    {#if removeError}<p class="failed" role="alert">{removeError}</p>{/if}
  </section>
</div>

<style>
  .settings {
    display: grid;
    gap: 36px;
    max-width: 760px;
  }

  header p {
    margin-top: 4px;
  }

  .form {
    display: grid;
    gap: 36px;
  }

  section {
    display: grid;
    gap: 14px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    align-items: start;
    gap: 14px 18px;
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

  .inline {
    display: flex;
    gap: 8px;
  }

  .inline input {
    flex: 1;
  }

  .hint {
    color: var(--muted);
    font-size: var(--step--1);
  }

  .hint.failed {
    color: var(--fail);
  }

  .grid + .hint,
  .block + .hint {
    margin-top: -6px;
  }

  input[type='color'] {
    width: 52px;
    height: 38px;
    padding: 3px;
    cursor: pointer;
  }

  .block {
    display: grid;
    gap: 6px;
  }

  .block textarea {
    font-family: var(--font-log);
    font-size: var(--step--1);
  }

  .removal {
    padding-top: 28px;
    border-top: 1px solid var(--line);
  }

  .path {
    overflow-wrap: anywhere;
  }

  .grid > .field .hint {
    display: block;
    margin-top: 2px;
  }
</style>
