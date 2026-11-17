<script lang="ts">
  import { loaderName } from '../lib/loader'
  import { onMount } from 'svelte'
  import { open } from '@tauri-apps/plugin-dialog'
  import { api, errorText, type InstanceInfo } from '../lib/api'
  import { job, onJobEnd } from '../lib/jobs.svelte'
  import { appSettingsView, nav } from '../lib/nav.svelte'
  import Notice from '../components/Notice.svelte'
  import LogView from '../components/LogView.svelte'

  let { first, onadded }: { first: boolean; onadded: () => void } = $props()

  let mode = $state<'track' | 'join'>('track')
  let instances: InstanceInfo[] = $state([])
  let loading = $state(true)
  let selected = $state<InstanceInfo | null>(null)
  let remote = $state('')
  let author = $state('')
  let projectId = $state('')
  let untrack = $state(true)
  let commitFirst = $state(true)
  let commitMessage = $state('')
  let commitAuthor = $state('')
  let error: string | null = $state(null)
  let pickError: string | null = $state(null)
  let started = $state(false)
  let result = $state<{ ok: boolean; message: string } | null>(null)
  let hasKey = $state(true)
  let hasGithubToken = $state(true)

  onMount(() => {
    discover()
    api.credentialsStatus().then((c) => {
      hasKey = c.builtInApiKey
      hasGithubToken = c.github.present
    }, () => {})
  })

  function chooseMode(next: 'track' | 'join') {
    mode = next
    selected = null
    result = null
    error = null
  }

  function selectable(i: InstanceInfo): boolean {
    return !i.added && !(mode === 'join' && i.hasGit)
  }

  async function discover() {
    loading = true
    try {
      instances = await api.discoverInstances()
      pickError = null
    } catch (e) {
      pickError = `Couldn't look for CurseForge instances: ${errorText(e)}`
    } finally {
      loading = false
    }
  }

  function select(instance: InstanceInfo) {
    selected = instance
    remote = mode === 'join' ? '' : (instance.remote ?? '')
    error = null
    pickError = null
  }

  async function chooseFolder() {
    const dir = await open({ directory: true })
    if (typeof dir !== 'string') return
    try {
      const instance = await api.inspectInstance(dir)
      if (!instances.some((i) => i.dir === instance.dir)) instances = [...instances, instance]
      select(instance)
    } catch (e) {
      pickError = `Couldn't read that folder as a CurseForge instance: ${errorText(e)}`
    }
  }

  async function setUp() {
    if (!selected) return
    try {
      error = null
      result = null
      started = true
      await api.setupInstance(selected.dir, { remote: remote.trim() || null, author, projectId: Number(projectId) || 0,
        untrackBinaries: untrack,
        commit: !selected.hasGit && commitFirst ? { message: commitMessage, author: commitAuthor.trim() || null } : null,
      })
    } catch (e) {
      started = false
      error = errorText(e)
    }
  }

  async function join() {
    if (!selected) return
    try {
      error = null
      result = null
      started = true
      await api.joinPack(selected.dir, remote.trim())
    } catch (e) {
      started = false
      error = errorText(e)
    }
  }

  // We show what setup did before moving on, since it can commit jars to git that you expected
  // it to leave out.
  onJobEnd('setup', (phase) => {
    if (!started) return
    started = false
    result = { ok: phase === 'done', message: job('setup').message ?? '' }
    if (selected) reinspect(selected.dir)
  })

  // A failed join or setup can stop after creating .git or mods.lock.json, so we refresh the status and
  // steps. We keep a selection that's no longer selectable, so its form and error stay in view.
  function reinspect(dir: string) {
    api.inspectInstance(dir).then((fresh) => {
      instances = instances.map((i) => (i.dir === dir ? fresh : i))
      if (selected?.dir === dir && selectable(fresh)) selected = fresh
    }, () => {})
  }

  function status(i: InstanceInfo): string {
    if (i.added) return 'Already added'
    if (mode === 'join') {
      if (i.hasGit) return 'Already a git repository'
      return i.addonCount ? `Has ${i.addonCount} CurseForge files` : 'Empty, ready to join'
    }
    if (i.hasLock && i.hooks === 'current') return 'Set up, ready to add'
    if (i.hasGit) return 'Git repository, needs setup'
    return 'Not set up yet'
  }

  let running = $derived(job('setup').phase === 'running')
  let steps = $derived.by(() => {
    if (!selected) return []
    const list: string[] = []
    if (!selected.hasGit) list.push('Create a git repository in the instance folder')
    if (selected.hooks === 'missing') list.push('Install the sync hooks (.githooks). Git runs these to download mods each time you pull, and to update the mod list (mods.lock.json) each time you commit')
    if (selected.hooks === 'outdated') list.push('Update the sync hooks (.githooks) to this version of packraven')
    list.push(selected.hasGit ? 'Add mod jars and pack zips to .gitignore' : 'Write a .gitignore that leaves out logs, saves, caches and mod jars')
    if (selected.missingMods) list.push(`Download the ${selected.missingMods} ${selected.missingMods === 1 ? 'file' : 'files'} in mods.lock.json that the mods folder doesn't have yet`)
    list.push(selected.hasLock ? 'Rebuild mods.lock.json from the mods folder' : `Write mods.lock.json from the ${selected.addonCount} CurseForge files, and track files not on CurseForge in git directly`)
    if (selected.hasGit) list.push("Stage these changes in git (you'll make the commits yourself)")
    return list
  })

  let joinSteps = $derived.by(() => {
    if (!selected) return []
    const list = [
      'Create a git repository in the profile folder and fetch the pack from the remote',
      "Check that the profile's Minecraft version and mod loader match the pack's, and stop without changing the profile if they don't",
      "Replace files the profile already has, like the options.txt CurseForge writes, with the pack's versions",
      'Link the sync hooks (.githooks). Git runs these to download mods each time you pull, and to update the mod list (mods.lock.json) each time you commit',
      "Download the mods and other files in the pack's mod list",
    ]
    if (selected.addonCount) list.push(`Set aside any of the profile's ${selected.addonCount} CurseForge files that the pack's mod list doesn't include`)
    return list
  })

  let removable = $derived(selected?.trackedBinaries.filter((b) => b.kind !== 'local') ?? [])
  let kept = $derived(selected?.trackedBinaries.filter((b) => b.kind === 'local') ?? [])
  const fileName = (path: string) => path.slice(path.lastIndexOf('/') + 1)
</script>

<div class="add">
  <div class="intro">
    <h2>{first ? 'Add your first pack' : 'Add a pack'}</h2>
    <p class="muted">packraven keeps mod jars out of the git repository and records them in a mod list (mods.lock.json) instead.</p>
  </div>

  <fieldset class="add-modes">
    <legend class="visually-hidden">How to add the pack</legend>
    <label class="add-mode" class:chosen={mode === 'track'} class:disabled={running}>
      <input type="radio" name="mode" checked={mode === 'track'} disabled={running} onchange={() => chooseMode('track')} />
      <span class="title">Track a CurseForge instance</span>
      <span class="muted">Start tracking a pack you already have in CurseForge.</span>
    </label>
    <label class="add-mode" class:chosen={mode === 'join'} class:disabled={running}>
      <input type="radio" name="mode" checked={mode === 'join'} disabled={running} onchange={() => chooseMode('join')} />
      <span class="title">Join a shared pack</span>
      <span class="muted">Download a pack someone else tracks in git into a new CurseForge profile.</span>
    </label>
  </fieldset>

  <fieldset class="instances">
    <legend>{mode === 'join' ? 'CurseForge profile' : 'CurseForge instance'}</legend>
    {#if mode === 'join'}
      <p class="lead pick-lead">
        Before you join, create a profile in the CurseForge app with the pack's Minecraft version and mod loader, and don't add any mods to it.
        Then choose it here (click Look again if it isn't listed yet).
      </p>
    {/if}
    {#if loading}
      <p class="muted">Looking for instances.</p>
    {:else if !pickError || instances.length}
      {#each instances as instance (instance.dir)}
        <label class="instance" class:chosen={selected?.dir === instance.dir} class:disabled={!selectable(instance)}>
          <input type="radio" name="instance" value={instance.dir} disabled={!selectable(instance) || running} checked={selected?.dir === instance.dir} onchange={() => select(instance)} />
          <span class="title">{instance.name}</span>
          <span class="muted">{instance.minecraft} {loaderName(instance.loader)}, {instance.addonCount} files</span>
          <span class="status">{status(instance)}</span>
        </label>
      {:else}
        <p class="muted">packraven found no CurseForge instances in the Instances folder. Choose a folder instead.</p>
      {/each}
    {/if}
    {#if pickError}
      <Notice tone="error">
        <p>{pickError}</p>
        {#if !instances.length && mode !== 'join'}<p><button type="button" onclick={discover}>Look again</button></p>{/if}
      </Notice>
    {/if}
    <div class="pick-actions">
      {#if mode === 'join'}<button type="button" onclick={discover} disabled={running || loading}>Look again</button>{/if}
      <button type="button" onclick={chooseFolder} disabled={running}>Choose another folder</button>
    </div>
  </fieldset>

  {#if selected && mode === 'join'}
    <form class="setup" onsubmit={(e) => { e.preventDefault(); join() }}>
      <fieldset>
        <legend>{selected.name}</legend>
        <p class="lead">When you click Join pack, packraven will:</p>
        <ul class="steps">
          {#each joinSteps as step (step)}<li><span class="marker" aria-hidden="true"></span>{step}</li>{/each}
        </ul>
      </fieldset>

      <fieldset>
        <legend>Pack to join</legend>
        <div class="field">
          <label for="join-remote">Git remote</label>
          <input id="join-remote" bind:value={remote} required spellcheck="false" autocomplete="off" placeholder="https://github.com/owner/pack.git" aria-describedby="join-remote-hint" />
          <p class="hint" id="join-remote-hint">
            The repository your collaborators push the pack to.
            {#if !hasGithubToken}
              If it's a private GitHub repository, <button type="button" class="link" onclick={() => { appSettingsView.tab = 'accounts'; nav.anchor = 'github-access-title'; nav.view = 'settings' }}>add a GitHub token in App settings</button> first.
            {/if}
          </p>
        </div>
      </fieldset>

      {#if error}<Notice tone="error"><p>{error}</p></Notice>{/if}
      {#if result && !result.ok}<Notice tone="error"><p>{result.message}</p></Notice>{/if}
      {#if result?.ok}
        <Notice tone="ok">
          <p>{result.message}</p>
          <p><button type="button" class="primary" onclick={onadded}>Open Mods</button></p>
        </Notice>
      {:else}
        <div class="actions">
          <button type="submit" class="primary" class:busy={running} disabled={running || !remote.trim()}>{running ? 'Joining' : 'Join pack'}</button>
        </div>
      {/if}
      {#if (started || result) && job('setup').lines.length}
        <LogView lines={job('setup').lines} label="Join log" />
      {/if}
    </form>
  {:else if selected}
    <form class="setup" onsubmit={(e) => { e.preventDefault(); setUp() }}>
      <fieldset>
        <legend>{selected.name}</legend>
        <p class="lead">When you click {selected.hasLock && selected.hooks === 'current' ? 'Add pack' : 'Set up'}, packraven will:</p>
        <ul class="steps">
          {#each steps as step (step)}<li><span class="marker" aria-hidden="true"></span>{step}</li>{/each}
          {#if removable.length}
            <li>
              <input id="untrack" type="checkbox" bind:checked={untrack} aria-describedby="untrack-files" />
              <div>
                <label for="untrack">
                  Remove {removable.length === 1 ? 'this file' : `these ${removable.length} files`} from git. From then on, the sync hooks download
                  {removable.length === 1 ? 'it' : 'them'} from CurseForge instead. The files stay on disk, and packraven won't edit past commits.
                </label>
                <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
                <ul class="files" id="untrack-files" tabindex="0" aria-label="Files to remove from git">{#each removable as file (file.path)}<li class="file">{fileName(file.path)}</li>{/each}</ul>
              </div>
            </li>
          {/if}
          {#if !selected.hasGit}
            <li>
              <input id="first-commit" type="checkbox" bind:checked={commitFirst} />
              <div class="commit">
                <label for="first-commit">Make the first commit with everything in the instance. Otherwise packraven only stages the files.</label>
                {#if commitFirst}
                  <div class="commit-fields">
                    <div class="field">
                      <label for="commit-message">Commit message <span class="optional">(optional, defaults to "Initial")</span></label>
                      <input id="commit-message" bind:value={commitMessage} placeholder="Initial" />
                    </div>
                    <div class="field">
                      <label for="commit-author">Commit author <span class="optional">(optional)</span></label>
                      <input id="commit-author" bind:value={commitAuthor} placeholder="Name <email>" aria-describedby="commit-author-hint" />
                      <p class="hint" id="commit-author-hint">Leave empty to use your git identity.</p>
                    </div>
                  </div>
                {/if}
              </div>
            </li>
          {/if}
          {#if !hasKey}
            <li class="caution">
              <span class="marker warn" aria-hidden="true"></span>
              <div>
                This packraven build has no CurseForge API key, so we can't recognize jars you added by hand and will track them in git
                directly instead of in the mod list. Build packraven with <code>PACKRAVEN_CF_API_KEY</code> set to keep them out of git.
              </div>
            </li>
          {/if}
          {#if kept.length}
            <li>
              <span class="marker quiet" aria-hidden="true"></span>
              <div>
                Keep {kept.length === 1 ? 'this jar' : `these ${kept.length} jars`} in git, since {kept.length === 1 ? "it isn't" : "they aren't"} on CurseForge:
                <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
                <ul class="files" tabindex="0" aria-label="Jars kept in git">{#each kept as file (file.path)}<li class="file">{fileName(file.path)}</li>{/each}</ul>
              </div>
            </li>
          {/if}
        </ul>
      </fieldset>

      <fieldset>
        <legend>Pack details</legend>
        <div class="fields">
          <div class="field wide">
            <label for="remote">Git remote <span class="optional">(optional)</span></label>
            <input id="remote" bind:value={remote} spellcheck="false" autocomplete="off" placeholder="https://github.com/you/pack.git" aria-describedby="remote-hint" />
            <p class="hint" id="remote-hint">Where you push your commits. packraven only pushes when you ask it to.</p>
          </div>
          <div class="field">
            <label for="author">Author <span class="optional">(optional)</span></label>
            <input id="author" bind:value={author} aria-describedby="author-hint" />
            <p class="hint" id="author-hint">packraven writes this as the pack author in the CurseForge export.</p>
          </div>
          <div class="field">
            <label for="project">CurseForge project ID <span class="optional">(optional)</span></label>
            <input id="project" inputmode="numeric" placeholder="8675309" bind:value={projectId} aria-describedby="project-hint" />
            <p class="hint" id="project-hint">Usually a series of numbers, e.g. 8675309. packraven needs it to publish; you can add it later in Pack settings.</p>
          </div>
        </div>
      </fieldset>

      {#if error}<Notice tone="error"><p>{error}</p></Notice>{/if}
      {#if result && !result.ok}<Notice tone="error"><p>{result.message}</p></Notice>{/if}
      {#if result?.ok}
        <Notice tone="ok">
          <p>{result.message}</p>
          <p><button type="button" class="primary" onclick={onadded}>Open Mods</button></p>
        </Notice>
      {:else}
        <div class="actions">
          <button type="submit" class="primary" class:busy={running} disabled={running}>
            {running ? 'Setting up' : selected.hasLock && selected.hooks === 'current' ? 'Add pack' : 'Set up'}
          </button>
        </div>
      {/if}
      {#if (started || result) && job('setup').lines.length}
        <LogView lines={job('setup').lines} label="Setup log" />
      {/if}
    </form>
  {/if}
</div>

<style>
  .add {
    display: grid;
    gap: 28px;
    max-width: 820px;
  }

  .intro p {
    margin-top: 4px;
  }

  fieldset {
    display: grid;
    gap: 10px;
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 0;
  }

  legend {
    padding: 0;
    margin-bottom: 10px;
    font-size: var(--step-1);
    font-weight: 700;
  }

  .instances {
    gap: 0;
  }

  .instance {
    display: grid;
    grid-template-columns: auto 1fr auto;
    grid-template-rows: auto auto;
    column-gap: 12px;
    align-items: center;
    padding: 12px 14px;
    color: var(--text);
    font-size: var(--step-0);
    border-top: 1px solid var(--line);
    cursor: pointer;
  }

  .instance:last-of-type {
    border-bottom: 1px solid var(--line);
  }

  .instance:hover:not(.disabled, .chosen) {
    background: var(--surface);
  }

  .instance.chosen {
    background: color-mix(in srgb, var(--forest) 12%, var(--surface));
  }

  .instance.disabled {
    cursor: default;
    opacity: 0.55;
  }

  .instance input {
    grid-row: span 2;
  }

  .instance .title {
    font-weight: 700;
  }

  .instance .muted {
    grid-column: 2;
    font-size: var(--step--1);
  }

  .status {
    grid-row: 1 / span 2;
    grid-column: 3;
    color: var(--muted);
    font-size: var(--step--1);
  }

  .add-modes {
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 12px;
  }

  .add-mode {
    display: grid;
    grid-template-columns: auto 1fr;
    column-gap: 10px;
    row-gap: 2px;
    align-items: baseline;
    align-content: start;
    padding: 12px 14px;
    border: 1px solid var(--line);
    border-radius: var(--radius-panel);
    color: var(--text);
    cursor: pointer;
  }

  .add-mode:hover:not(.disabled, .chosen) {
    background: var(--surface);
  }

  .add-mode.disabled {
    cursor: default;
  }

  .add-mode.chosen {
    background: color-mix(in srgb, var(--forest) 12%, var(--surface));
  }

  .add-mode .title {
    font-weight: 700;
  }

  .add-mode .muted {
    grid-column: 2;
    font-size: var(--step--1);
  }

  .pick-actions {
    display: flex;
    gap: 12px;
  }

  .instances > div,
  .instances > :global(.notice) {
    margin-top: 14px;
  }

  @media (max-width: 800px) {
    .add-modes,
    .fields,
    .commit-fields {
      grid-template-columns: minmax(0, 1fr);
    }
  }

  .setup {
    display: grid;
    gap: 18px;
  }

  .lead {
    color: var(--muted);
  }

  .pick-lead {
    margin: 0 0 14px;
  }

  .steps {
    display: grid;
    gap: 8px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .steps > li {
    display: grid;
    grid-template-columns: 18px 1fr;
    column-gap: 10px;
    align-items: baseline;
  }

  .marker {
    justify-self: center;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--forest);
    transform: translateY(-2px);
  }

  .marker.quiet {
    background: var(--muted);
  }

  .marker.warn {
    background: var(--warn-icon);
  }

  .caution {
    color: var(--warn);
  }

  .files {
    margin: 4px 0 0;
    padding: 0;
    list-style: none;
    color: var(--muted);
    max-height: 140px;
    overflow: auto;
  }

  .commit-fields {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 14px 20px;
    margin-top: 12px;
  }

  .steps > li > input {
    justify-self: center;
    margin: 0;
    transform: translateY(2px);
  }

  .steps label {
    color: var(--text);
    font-size: var(--step-0);
  }

  .fields {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 18px 20px;
  }

  .field {
    display: grid;
    gap: 6px;
    align-content: start;
    min-width: 0;
  }

  .field.wide {
    grid-column: 1 / -1;
  }

  .field label {
    display: block;
    color: var(--text);
    font-size: var(--step-0);
  }

  .field input {
    width: 100%;
    min-width: 0;
  }

  .optional {
    color: var(--muted);
    font-size: var(--step--1);
  }

  .hint {
    margin: 0;
    color: var(--muted);
    font-size: var(--step--1);
  }

  .actions {
    display: flex;
    gap: 12px;
  }
</style>
