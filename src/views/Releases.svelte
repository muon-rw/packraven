<script lang="ts">
  import { onMount, tick } from 'svelte'
  import { listen } from '@tauri-apps/api/event'
  import { ask } from '@tauri-apps/plugin-dialog'
  import { openUrl } from '@tauri-apps/plugin-opener'
  import { api, errorText, type FileState, type PackFile, type ReleaseStatus, type UpdateCheckerSetup, type UpdateType } from '../lib/api'
  import ServerPackSteps from '../components/ServerPackSteps.svelte'
  import Notice from '../components/Notice.svelte'
  import { job, onJobEnd } from '../lib/jobs.svelte'
  import { asReleaseType, asUpdateType, curseforgeProblem, repoName, updateTypeLabel } from '../lib/format'

  let { onsettings, onappsettings }: { onsettings: () => void; onappsettings: () => void } = $props()

  let releases: ReleaseStatus[] = $state([])
  let releasesError: string | null = $state(null)
  let loading = $state(true)
  let hasWebhook = $state(false)
  let announcing = $state<string | null>(null)
  let discordError = $state<string | null>(null)
  let rolePing = $state(false)
  let uploading = $state<string | null>(null)
  let uploadErrors = $state<Record<string, string>>({})
  let updateChecker = $state<UpdateCheckerSetup | null>(null)
  let defaultUpdateType = $state<UpdateType>('minor_breaking')
  let addingToChecker = $state<string | null>(null)
  // A failure the backend couldn't record on the release, such as one from before it found the release.
  let checkerErrors = $state<Record<string, string>>({})
  // Errors already on the page stay quiet when Releases opens, so we only announce the outcome of an add you started.
  let checkerAnnouncement = $state('')
  // The file we're replacing or checking, as "version:client" or "version:server", and what came of it.
  let replacing = $state<string | null>(null)
  let checking = $state<string | null>(null)
  let replaceErrors = $state<Record<string, string>>({})
  let replaced = $state<string | null>(null)
  let replacedNotice = $state<HTMLElement>()

  type Problem = { title: string; detail: string; before: string[]; deleteFirst: boolean; waitable: boolean }

  function day(iso: string | null) {
    if (!iso) return null
    const at = new Date(iso)
    const days = Math.floor((Date.now() - at.getTime()) / 86_400_000)
    return `${at.toLocaleDateString()} (${days === 0 ? 'today' : days === 1 ? 'yesterday' : `${days} days ago`})`
  }

  // The states where a file waits on you rather than on CurseForge, and what we tell you to do. The steps
  // in `before` come ahead of the upload, and the first one opens the file on CurseForge.
  function problem(state: FileState | null, file: PackFile, version: string, uploaded: string | null): Problem | null {
    const since = day(uploaded)
    const what = `${file} pack`
    const title = curseforgeProblem(state, version, file) ?? ''
    switch (state) {
      case 'manualReview':
        return {
          title,
          detail: `${since ? `Uploaded ${since}. ` : ''}Moderators can take weeks. You can keep waiting, or delete it on CurseForge and upload it again, which will start its review over.`,
          before: [`Delete it there.${what === 'client pack' ? ' The server pack will go with it.' : ' Leave the client pack as it is.'}`],
          deleteFirst: true,
          waitable: true,
        }
      case 'changesRequired':
        return {
          title,
          detail: '',
          before: ["Read the moderator's notes, then delete the file there.", 'Make those changes to the pack.'],
          deleteFirst: true,
          waitable: false,
        }
      case 'rejected':
        return { title, detail: "The moderator's reason is usually given in your author notifications.", before: ['See why.'], deleteFirst: false, waitable: false }
      case 'malwareDetected':
        return {
          title,
          detail: 'If you think CurseForge got it wrong, contact CurseForge support before you upload it again.',
          before: ['See the details.'],
          deleteFirst: false,
          waitable: false,
        }
      case 'failedPublishing':
        return { title, detail: 'This failed on their side, so you should try uploading it again.', before: [], deleteFirst: false, waitable: false }
      case 'deleted':
        return { title, detail: '', before: [], deleteFirst: false, waitable: false }
      default:
        return null
    }
  }

  // You can choose to wait out a moderator, so we fold that notice away until its state changes.
  const waitingKey = (release: ReleaseStatus, file: PackFile) => `packraven.waiting.${release.curseforge?.projectId}.${release.version}.${file}`
  let waiting = $state<Record<string, boolean>>({})
  function isWaiting(release: ReleaseStatus, file: PackFile) {
    const key = waitingKey(release, file)
    return waiting[key] ?? localStorage.getItem(key) === '1'
  }
  function setWaiting(release: ReleaseStatus, file: PackFile, on: boolean) {
    const key = waitingKey(release, file)
    if (on) localStorage.setItem(key, '1')
    else localStorage.removeItem(key)
    waiting = { ...waiting, [key]: on }
  }
  let releaseType = $state('release')
  let showCurseforge = $derived(releases.some((r) => r.curseforge))
  let showModrinth = $derived(releases.some((r) => r.modrinth || r.modrinthMissing))
  let checkerRepo = $derived(updateChecker?.kind === 'github' ? updateChecker.repo : null)
  let showUpdateChecker = $derived(!!checkerRepo || releases.some((r) => r.updateChecker))
  let columns = $derived(2 + (showCurseforge ? 1 : 0) + (showModrinth ? 1 : 0) + (showUpdateChecker ? 1 : 0))

  onMount(() => {
    loadReleases()
    api.getWebhook().then((w) => (hasWebhook = !!w), () => (hasWebhook = false))
    api.getPackConfig().then((c) => {
      rolePing = !!c.discord.role_id.trim()
      releaseType = c.curseforge.release_type
      defaultUpdateType = c.update_checker.update_type
    }, () => {})
    api.updateCheckerSetup().then((setup) => (updateChecker = setup), () => (updateChecker = null))
    const unlisten = [listen('release-announced', () => loadReleases()), listen('release-update-checker', () => loadReleases())]
    return () => {
      for (const stop of unlisten) stop.then((stop) => stop())
    }
  })

  // Retrying a Modrinth upload or replacing a file runs as a publish job, so we hear how it went the same way.
  onJobEnd('publish', async (phase) => {
    if (replacing) {
      const key = replacing
      replaceErrors = { ...replaceErrors, [key]: phase === 'failed' ? (job('publish').message ?? 'The upload failed') : '' }
      replacing = null
      if (phase === 'done') {
        const [version, file] = key.split(':')
        replaced = file === 'client' ? `Uploaded a new ${version} client pack and its server pack. CurseForge will review them again.` : `Uploaded a new ${version} server pack. CurseForge will review it again.`
        await tick()
        replacedNotice?.focus()
      }
    }
    if (uploading) {
      const failure = phase === 'failed' ? `Couldn't upload it: ${job('publish').message ?? 'the upload failed'}` : null
      uploadErrors = { ...uploadErrors, [uploading]: failure ?? '' }
      uploading = null
    }
    if (phase === 'done') loadReleases()
  })

  // An upload is public and can't be taken back, so we say what goes up before it does.
  async function uploadToModrinth(release: ReleaseStatus) {
    const { version } = release
    const name = release.displayName ? `"${release.displayName}"` : `the ${version} pack`
    const confirmed = await ask(
      `Upload ${name} (${release.modrinthMissing}) to Modrinth as ${asReleaseType(release.releaseType ?? releaseType)}, with the notes packraven archived for ${version}. Modrinth usually lists it right away.\n\nThis can't be undone.`,
      { title: `Upload ${version} to Modrinth`, kind: 'warning', okLabel: 'Upload to Modrinth', cancelLabel: 'Cancel' },
    )
    if (!confirmed) return
    uploadErrors = { ...uploadErrors, [version]: '' }
    uploading = version
    try {
      await api.uploadReleaseToModrinth(version)
    } catch (e) {
      uploadErrors = { ...uploadErrors, [version]: `Couldn't upload it: ${errorText(e)}` }
      uploading = null
    }
  }

  // We only offer the upload once CurseForge no longer has the old file, so this asks CurseForge again.
  async function checkGone(release: ReleaseStatus, file: PackFile) {
    const key = `${release.version}:${file}`
    checking = key
    replaceErrors = { ...replaceErrors, [key]: '' }
    await loadReleases()
    checking = null
    const still = releases.find((r) => r.version === release.version)
    const found = still && fileProblems(still).find((p) => p.file === file)
    if (found?.found.deleteFirst) replaceErrors = { ...replaceErrors, [key]: `CurseForge still has the ${file} pack. Delete it there, then check again.` }
  }

  // An upload is public and can't be taken back, so we say what goes up, and what it's built from, first.
  async function replaceFile(release: ReleaseStatus, file: PackFile) {
    const { version } = release
    const name = release.displayName ?? version
    const rebuildsClient = file === 'client' && !release.clientZipPresent
    const lines = [
      file === 'client'
        ? rebuildsClient
          ? `packraven no longer has the client pack it built for ${version}, so we build it again from the pack as it is now, upload it to CurseForge, then upload a new server pack for it.`
          : `Upload the ${version} client pack to CurseForge again, then a new server pack for it, built from the pack as it is now.`
        : `Build a new server pack from the pack as it is now, and upload it to CurseForge as an additional file of ${name}.`,
      'CurseForge reviews what we upload again.',
      release.packChanged
        ? `Your mods changed since ${version} shipped, so ${rebuildsClient ? "the new packs won't match what players who already downloaded " + version + ' have. Consider changing the version number.' : "the new server pack won't match the client pack"}.`
        : null,
    ].filter(Boolean)
    const confirmed = await ask(`${lines.join('\n\n')}\n\nThis can't be undone.`, {
      title: `Upload a new ${version} ${file} pack`,
      kind: 'warning',
      okLabel: release.packChanged ? 'Upload anyway' : `Upload new ${file} pack`,
      cancelLabel: 'Cancel',
    })
    if (!confirmed) return
    const key = `${version}:${file}`
    replaceErrors = { ...replaceErrors, [key]: '' }
    replaced = null
    replacing = key
    try {
      await api.replaceCurseforgeFile(version, file)
    } catch (e) {
      replaceErrors = { ...replaceErrors, [key]: errorText(e) }
      replacing = null
    }
  }

  // The client pack's problem covers its server pack too, since replacing it replaces both.
  function fileProblems(release: ReleaseStatus): { file: PackFile; page: string; found: Problem }[] {
    const curseforge = release.curseforge
    if (!curseforge) return []
    const client = problem(curseforge.state, 'client', release.version, curseforge.uploaded)
    if (client) return [{ file: 'client', page: curseforge.page, found: client }]
    const server = curseforge.serverPage ? problem(curseforge.serverState, 'server', release.version, curseforge.serverUploaded) : null
    return server ? [{ file: 'server', page: curseforge.serverPage!, found: server }] : []
  }

  function fileTone(state: FileState | null) {
    switch (state) {
      case 'approved':
        return 'ok'
      case 'rejected':
      case 'malwareDetected':
      case 'failedPublishing':
      case 'deleted':
      case 'changesRequired':
        return 'fail'
      // Retired files are your choice, and a file we couldn't check tells us nothing, so neither warns.
      case 'retired':
      case null:
        return 'change'
      default:
        return 'pending'
    }
  }

  // Modrinth lists a version as soon as we upload it, so only CurseForge's review holds a release back.
  function holdups(release: ReleaseStatus): string[] {
    const { curseforge, modrinth } = release
    return [
      curseforge && fileTone(curseforge.state) !== 'ok' ? `CurseForge still lists the file as "${curseforge.status}"` : null,
      release.serverPackMissing ? "its server pack isn't on CurseForge yet" : null,
      curseforge && fileProblems(release).some((p) => p.file === 'server') ? 'CurseForge has a problem with its server pack' : null,
      release.modrinthMissing ? "it isn't on Modrinth yet" : null,
      modrinth && statusTone(modrinth.status) !== 'ok' ? `Modrinth lists it as "${modrinth.status}"` : null,
    ].filter((reason) => reason !== null)
  }

  function approved(release: ReleaseStatus) {
    return holdups(release).length === 0
  }

  // The background check waits for the same things before it announces or adds a release.
  function approvalWait(release: ReleaseStatus) {
    const curseforge = release.serverPackMissing ? 'CurseForge approves it and has its server pack' : 'CurseForge approves it'
    return [release.curseforge && curseforge, release.modrinthMissing && "it's on Modrinth"].filter(Boolean).join(', and ')
  }

  function updateCheckerStatus(release: ReleaseStatus) {
    const entry = release.updateChecker
    if (!entry) return 'Not added'
    if (entry.addedAt) return `Added ${new Date(entry.addedAt).toLocaleDateString()}`
    if (checkerError(release)) return "Couldn't add it"
    const wait = approvalWait(release)
    return wait ? `packraven adds this once ${wait}` : 'packraven adds this within a few minutes'
  }

  function checkerError(release: ReleaseStatus) {
    return release.updateChecker?.addedAt ? null : (checkerErrors[release.version] ?? release.updateChecker?.error ?? null)
  }

  // Adding a release tells players to update, so we ask first when it isn't out everywhere yet, or when it never
  // had an update type of its own.
  async function addToUpdateChecker(release: ReleaseStatus) {
    const { version } = release
    const type = release.updateChecker?.updateType ?? defaultUpdateType
    const where = checkerRepo ? repoName(checkerRepo) : 'the update checker'
    const lines = [
      release.updateChecker ? null : `Add ${version} to ${where} as ${asUpdateType(type)}, the default from Pack settings.`,
      approved(release) ? null : `Players will see ${version} as an update in game right away, though ${holdups(release).join(', and ')}.`,
    ].filter(Boolean)
    if (lines.length) {
      const confirmed = await ask(lines.join('\n\n'), {
        title: `Add ${version} to the update checker`,
        kind: 'warning',
        okLabel: 'Add it',
        cancelLabel: 'Cancel',
      })
      if (!confirmed) return
    }
    addingToChecker = version
    checkerAnnouncement = ''
    try {
      checkerAnnouncement = await api.addToUpdateChecker(version)
      delete checkerErrors[version]
    } catch (e) {
      checkerErrors[version] = errorText(e)
      checkerAnnouncement = `Couldn't add ${version} to the update checker. ${errorText(e)}`
    } finally {
      addingToChecker = null
      await loadReleases()
    }
  }

  async function loadReleases() {
    loading = true
    try {
      releases = await api.releases()
      releasesError = null
    } catch (e) {
      releasesError = `Couldn't load releases: ${errorText(e)}`
    } finally {
      loading = false
    }
  }

  // An announcement is public and can ping a whole role, so we ask first and say when CurseForge
  // hasn't approved the file yet.
  async function announceNow(release: ReleaseStatus) {
    const { version } = release
    const ready = approved(release)
    const lines = [
      `Post the ${version} announcement to the Discord channel your webhook points at${rolePing ? ', then ping the role from Pack settings' : ''}.`,
      ready ? null : `${holdups(release).join(', and ')}, so players may not be able to download it everywhere yet.`,
      release.announcedAt ? `You already announced it on ${new Date(release.announcedAt).toLocaleDateString()}, so this will post it again.` : null,
    ].filter(Boolean)
    const confirmed = await ask(`${lines.join('\n\n')}\n\nYou can't unsend it from packraven.`, {
      title: ready ? `Announce ${version}` : `Announce ${version} before approval`,
      kind: 'warning',
      okLabel: release.announcedAt ? 'Announce again' : 'Announce',
      cancelLabel: 'Cancel',
    })
    if (!confirmed) return
    announcing = version
    try {
      await api.announceRelease(version)
      discordError = null
      await loadReleases()
    } catch (e) {
      discordError = errorText(e)
    } finally {
      announcing = null
    }
  }

  async function editInDiscohook(version: string) {
    try {
      openUrl(await api.discohookLink(version))
      discordError = null
    } catch (e) {
      discordError = `Couldn't open Discohook: ${errorText(e)}`
    }
  }

  function discordState(release: ReleaseStatus) {
    if (release.announcedAt) return `Announced ${new Date(release.announcedAt).toLocaleDateString()}`
    if (release.announce) {
      const wait = approvalWait(release)
      return wait ? `packraven posts this once ${wait}` : 'packraven posts this within a few minutes'
    }
    return 'Not announced'
  }

  function statusTone(status: string) {
    return /approved|released|listed/i.test(status) ? 'ok' : /reject|delete|fail/i.test(status) ? 'fail' : 'pending'
  }
</script>

<div class="releases">
  <header class="head">
    <div>
      <h2>Releases</h2>
      <p class="muted">
        Versions published from packraven on this machine, their status on each platform, {showUpdateChecker ? 'Discord announcements, and Modpack Update Checker entries' : 'and Discord announcements'}.
        {#if !hasWebhook}No Discord webhook is set, so packraven won't announce anything.{/if}
      </p>
    </div>
    <div class="actions">
      <button class="ghost settings" onclick={onsettings}>{showUpdateChecker ? 'Announcement settings' : 'Discord settings'}</button>
      <button class:busy={loading} disabled={loading} onclick={loadReleases}>{loading ? 'Checking' : 'Refresh'}</button>
    </div>
  </header>

  {#if releasesError}<p class="failed" role="alert">{releasesError}</p>{/if}
  {#if discordError}<p class="failed" role="alert">{discordError}</p>{/if}
  <p class="visually-hidden" role="status">{checkerAnnouncement}</p>
  <!-- The problem notice you acted on goes away, so this takes its place and takes focus. -->
  {#if replaced}
    <div class="replaced" tabindex="-1" bind:this={replacedNotice}>
      <Notice tone="ok"><p>{replaced}</p></Notice>
    </div>
  {/if}

  {#if releases.length}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex (a scrollable region needs focus so you can scroll it with keys) -->
    <div class="scroll" role="region" aria-label="Releases" tabindex="0">
    <table>
      <thead>
        <tr>
          <th>Version</th>
          {#if showCurseforge}<th>CurseForge</th>{/if}
          {#if showModrinth}<th>Modrinth</th>{/if}
          <th>Discord</th>
          {#if showUpdateChecker}<th>Update Checker</th>{/if}
        </tr>
      </thead>
      <tbody>
        {#each [...releases].reverse() as release (release.version)}
          <tr>
            <td>
              <span class="file">{release.version}</span>
              <span class="published muted"><span class="visually-hidden">Published </span>{new Date(release.publishedAt).toLocaleDateString()}</span>
            </td>
            {#if showCurseforge}
              <td>
                {#if release.curseforge}
                  {@const curseforge = release.curseforge}
                  <dl class="statuses">
                    <dt>Client pack</dt>
                    <dd><span class="chip {fileTone(curseforge.state)}">{curseforge.status}</span></dd>
                    <dt>Server pack</dt>
                    <dd>
                      <!-- A missing server pack waits on you, not on CurseForge's review. -->
                      <span class="chip {release.serverPackMissing ? 'fail' : fileTone(curseforge.serverState)}">{curseforge.serverStatus}</span>
                      {#if curseforge.serverPackLinked === false}<span class="chip pending">Not linked as the server pack</span>{/if}
                    </dd>
                  </dl>
                {:else}
                  <span class="muted">Not published there</span>
                {/if}
              </td>
            {/if}
            {#if showModrinth}
              <td>
                {#if release.modrinth}
                  {@const modrinth = release.modrinth}
                  <div class="modrinth">
                    <span class="chip {statusTone(modrinth.status)}">{modrinth.status}</span>
                    <a href={modrinth.url} onclick={(e) => { e.preventDefault(); openUrl(modrinth.url) }}>View<span class="visually-hidden"> {release.version} on Modrinth</span></a>
                  </div>
                {:else if release.modrinthMissing}
                  <div class="stacked">
                    <span class="muted">Not uploaded</span>
                    <span class="cell-actions">
                      <button class:busy={uploading === release.version} disabled={uploading !== null} onclick={() => uploadToModrinth(release)}>
                        {uploading === release.version ? 'Uploading' : 'Upload to Modrinth'}<span class="visually-hidden"> {release.version}</span>
                      </button>
                    </span>
                    {#if uploadErrors[release.version]}<span class="failed small" role="alert">{uploadErrors[release.version]}</span>{/if}
                  </div>
                {:else}
                  <span class="muted">Not published there</span>
                {/if}
              </td>
            {/if}
            <td>
              <div class="stacked">
                <span class:muted={!release.announcedAt}>{discordState(release)}</span>
                <span class="cell-actions">
                  {#if hasWebhook}
                    <button class="ghost" class:busy={announcing === release.version} disabled={announcing !== null} onclick={() => announceNow(release)}>
                      {release.announcedAt ? 'Announce again' : approved(release) ? 'Announce now' : 'Announce before approval'}
                    </button>
                  {/if}
                  <button class="ghost" onclick={() => editInDiscohook(release.version)} aria-describedby="discohook-hint">Edit in Discohook</button>
                </span>
              </div>
            </td>
            {#if showUpdateChecker}
              {@const entry = release.updateChecker}
              {@const failed = !!checkerError(release)}
              <td>
                <div class="stacked">
                  {#if entry}<span>{updateTypeLabel(entry.updateType)} update</span>{/if}
                  <span class:muted={!entry?.addedAt && !failed} class:failed>{updateCheckerStatus(release)}</span>
                  {#if checkerRepo && !entry?.addedAt && !failed}
                    <span class="cell-actions">
                      <button class="ghost" class:busy={addingToChecker === release.version} disabled={addingToChecker !== null} onclick={() => addToUpdateChecker(release)}>
                        {#if addingToChecker === release.version}
                          Adding<span class="visually-hidden"> {release.version} to the update checker</span>
                        {:else}
                          Add<span class="visually-hidden"> {release.version} to the update checker</span>{entry ? ' now' : ''}
                        {/if}
                      </button>
                    </span>
                  {/if}
                </div>
              </td>
            {/if}
          </tr>
          {@const problems = fileProblems(release)}
          {#each problems as { file, page, found } (file)}
            {@const key = `${release.version}:${file}`}
            {@const label = `${release.version} ${file} pack`}
            <tr class="follow-up">
              <td colspan={columns}>
                {#if found.waitable && isWaiting(release, file)}
                  <p class="waiting muted small">
                    Waiting for a moderator to review the {label}.
                    <button class="link" onclick={() => setWaiting(release, file, false)}>Show options<span class="visually-hidden"> for the {label}</span></button>
                  </p>
                {:else}
                  <section class="server-upload" aria-labelledby="problem-{release.version}-{file}">
                    <Notice tone="warn" live={false}>
                      <h3 id="problem-{release.version}-{file}">{found.title}</h3>
                      {#if found.detail}<p class="muted">{found.detail}</p>{/if}
                      <!-- A list of one step only adds a number, so a lone upload gets a plain line. -->
                      <svelte:element this={found.before.length ? 'ol' : 'div'} class={found.before.length ? 'steps' : 'single'}>
                        {#each found.before as step, index (index)}
                          <li>
                            <div class="step">
                              {#if index === 0}<button onclick={() => openUrl(page)}>Open it on CurseForge<span class="visually-hidden"> ({label})</span></button>{/if}
                              <span class:muted={index === 0} class:small={index === 0}>{step}</span>
                            </div>
                          </li>
                        {/each}
                        <svelte:element this={found.before.length ? 'li' : 'div'}>
                          <div class="step">
                            {#if found.deleteFirst}
                              <button class:busy={checking === key} disabled={checking !== null} onclick={() => checkGone(release, file)}>
                                {checking === key ? 'Checking' : "Check that it's gone"}<span class="visually-hidden"> ({label})</span>
                              </button>
                              <span class="muted small">Once CurseForge no longer has it, we offer to upload a new one.</span>
                            {:else}
                              <button class:busy={replacing === key} disabled={job('publish').phase === 'running'} onclick={() => replaceFile(release, file)}>
                                {replacing === key ? 'Uploading' : `Upload a new ${file} pack`}<span class="visually-hidden"> for {release.version}</span>
                              </button>
                              <span class="muted small">
                                {file === 'client' ? 'We upload the client pack again, then a new server pack built from the pack as it is now.' : 'We build it from the pack as it is now.'}
                              </span>
                            {/if}
                          </div>
                        </svelte:element>
                      </svelte:element>
                      {#if release.packChanged}<p class="muted small">Your mods changed since {release.version} shipped, so a new server pack won't match the client pack.</p>{/if}
                      {#if replaceErrors[key]}<p class="failed small" role="alert">{replaceErrors[key]}</p>{/if}
                      {#if found.waitable}<p><button class="link" onclick={() => setWaiting(release, file, true)}>Keep waiting<span class="visually-hidden"> for the {label}</span></button></p>{/if}
                    </Notice>
                  </section>
                {/if}
              </td>
            </tr>
          {/each}
          {@const checkerProblem = showUpdateChecker ? checkerError(release) : null}
          {#if checkerProblem}
            <tr class="follow-up">
              <td colspan={columns}>
                <section class="server-upload" aria-labelledby="checker-problem-{release.version}">
                  <Notice tone="warn" live={false}>
                    <h3 id="checker-problem-{release.version}">Couldn't add {release.version} to the update checker</h3>
                    <p class="muted">{checkerProblem}</p>
                    <div class="checker-actions">
                      <button class:busy={addingToChecker === release.version} disabled={addingToChecker !== null} onclick={() => addToUpdateChecker(release)}>
                        {#if addingToChecker === release.version}
                          Adding<span class="visually-hidden"> {release.version} to the update checker</span>
                        {:else}
                          Try<span class="visually-hidden"> adding {release.version} to the update checker</span> again
                        {/if}
                      </button>
                      <button class="link" onclick={onappsettings}>Open App settings</button>
                      {#if checkerRepo}
                        {@const repo = checkerRepo}
                        <button class="link" onclick={() => openUrl(repo.page)}>Open {repoName(repo)} on GitHub</button>
                      {/if}
                    </div>
                  </Notice>
                </section>
              </td>
            </tr>
          {/if}
          {#if release.serverPackMissing && !problems.some((p) => p.file === 'client')}
            <tr class="follow-up">
              <td colspan={columns}>
                <section class="server-upload" aria-labelledby="server-upload-{release.version}">
                  <Notice tone="warn" live={false}>
                    <h3 id="server-upload-{release.version}">Upload the {release.version} server pack</h3>
                    <ServerPackSteps upload={release.serverPackMissing} version={release.version} clientName={release.displayName ?? release.version} onmarked={loadReleases} />
                  </Notice>
                </section>
              </td>
            </tr>
          {/if}
        {/each}
      </tbody>
    </table>
    </div>
    <p class="muted small" id="discohook-hint">
      Discohook is a web editor for Discord messages. packraven opens the announcement there, so you can change it and post it yourself.
    </p>
  {:else if loading}
    <p class="muted" role="status">Checking each release's status.</p>
  {:else if !releasesError}
    <p class="muted">Releases you publish from packraven show up here with their status on each platform.</p>
  {/if}
</div>

<style>
  .releases {
    display: grid;
    gap: 22px;
  }

  .head {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: end;
    gap: 16px 24px;
  }

  .head p {
    margin-top: 4px;
  }

  .actions {
    display: flex;
    gap: 6px;
  }

  .settings {
    color: var(--forest);
  }

  .small {
    font-size: var(--step--1);
  }

  /* In a narrow pane the table scrolls sideways on its own, so the rest of the view keeps its width. */
  /* The positioned ancestor keeps the visually hidden button text inside the scroll region, or it widens the window. */
  .scroll {
    position: relative;
    overflow-x: auto;
    container-type: inline-size;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-variant-numeric: tabular-nums;
  }

  th {
    text-align: left;
    font-size: var(--step--1);
    font-weight: 400;
    color: var(--muted);
    padding: 6px 12px 6px 0;
  }

  td {
    padding: 8px 12px 8px 0;
    border-top: 1px solid var(--line);
    vertical-align: top;
  }

  .stacked {
    display: grid;
    gap: 2px;
  }

  .published {
    display: block;
    font-size: var(--step--1);
  }

  .statuses {
    display: grid;
    grid-template-columns: auto auto;
    justify-content: start;
    align-items: baseline;
    gap: 4px 8px;
    margin: 0;
  }

  .statuses dt {
    color: var(--muted);
    font-size: var(--step--1);
    white-space: nowrap;
  }

  .statuses dd {
    margin: 0;
  }

  /* The steps span the row under their release, so the table keeps its columns narrow. */
  .follow-up td {
    border-top: 0;
    padding-top: 0;
  }

  /* Sticky to the scroll area's left edge, and no wider than it, so the steps stay in view while the table scrolls sideways. */
  .server-upload {
    position: sticky;
    left: 0;
    max-width: min(80ch, 100cqi);
  }

  .server-upload h3 {
    font-size: var(--step-0);
  }

  .steps {
    display: grid;
    gap: 10px;
    margin: 0;
    padding-left: 20px;
  }

  .steps li {
    padding-left: 4px;
  }

  /* The list item keeps display: list-item so its number shows, and the step lays out inside it. */
  .step {
    display: grid;
    justify-items: start;
    gap: 4px;
  }

  .checker-actions {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 16px;
  }

  .waiting {
    position: sticky;
    left: 0;
    margin: 0;
  }

  .steps li::marker {
    color: var(--muted);
    font-size: var(--step--1);
  }

  .modrinth {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 10px;
  }

  .cell-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
    margin-left: -8px;
  }

  .cell-actions button {
    color: var(--forest);
    white-space: nowrap;
  }
</style>
