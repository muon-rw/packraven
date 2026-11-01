<script lang="ts">
  import { onMount } from 'svelte'
  import { listen } from '@tauri-apps/api/event'
  import { ask } from '@tauri-apps/plugin-dialog'
  import { openUrl } from '@tauri-apps/plugin-opener'
  import { api, errorText, type ReleaseStatus } from '../lib/api'
  import ServerPackSteps from '../components/ServerPackSteps.svelte'
  import Notice from '../components/Notice.svelte'
  import { job, onJobEnd } from '../lib/jobs.svelte'
  import { asReleaseType } from '../lib/format'

  let { onsettings }: { onsettings: () => void } = $props()

  let releases: ReleaseStatus[] = $state([])
  let releasesError: string | null = $state(null)
  let loading = $state(true)
  let hasWebhook = $state(false)
  let announcing = $state<string | null>(null)
  let discordError = $state<string | null>(null)
  let rolePing = $state(false)
  let uploading = $state<string | null>(null)
  let uploadErrors = $state<Record<string, string>>({})
  let releaseType = $state('release')
  let showCurseforge = $derived(releases.some((r) => r.curseforge))
  let showModrinth = $derived(releases.some((r) => r.modrinth || r.modrinthMissing))

  onMount(() => {
    loadReleases()
    api.getWebhook().then((w) => (hasWebhook = !!w), () => (hasWebhook = false))
    api.getPackConfig().then((c) => {
      rolePing = !!c.discord.role_id.trim()
      releaseType = c.curseforge.release_type
    }, () => {})
    const unlisten = listen('release-announced', () => loadReleases())
    return () => {
      unlisten.then((stop) => stop())
    }
  })

  // Retrying a Modrinth upload runs as a publish job, so we hear how it went the same way.
  onJobEnd('publish', (phase) => {
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
      `Upload ${name} (${release.modrinthMissing}) to Modrinth as ${asReleaseType(release.releaseType ?? releaseType)}, with the notes packraven archived for ${version}. Modrinth lists it right away.\n\nThis can't be undone.`,
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

  // Modrinth lists a version as soon as we upload it, so only CurseForge's review holds a release back.
  function holdups(release: ReleaseStatus): string[] {
    const { curseforge, modrinth } = release
    return [
      curseforge && statusTone(curseforge.status) !== 'ok' ? `CurseForge still lists the file as "${curseforge.status}"` : null,
      release.serverPackMissing ? "its server pack isn't on CurseForge yet" : null,
      release.modrinthMissing ? "it isn't on Modrinth yet" : null,
      modrinth && statusTone(modrinth.status) !== 'ok' ? `Modrinth lists it as "${modrinth.status}"` : null,
    ].filter((reason) => reason !== null)
  }

  function approved(release: ReleaseStatus) {
    return holdups(release).length === 0
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
      `Post the ${version} announcement to the Discord channel your webhook points at${rolePing ? ', pinging the role from Pack settings' : ''}.`,
      ready ? null : `${holdups(release).join(', and ')}, so players may not be able to download it everywhere yet.`,
      release.announcedAt ? `You already announced it on ${new Date(release.announcedAt).toLocaleDateString()}, so this posts it again.` : null,
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
      const curseforge = release.serverPackMissing ? 'CurseForge approves it and has its server pack' : 'CurseForge approves it'
      const waiting = [release.curseforge && curseforge, release.modrinthMissing && "it's on Modrinth"].filter(Boolean)
      return waiting.length ? `packraven posts this once ${waiting.join(', and ')}` : 'packraven posts this within a few minutes'
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
        Versions published from packraven on this machine, their status on each platform, and Discord announcements.
        {#if !hasWebhook}No Discord webhook is set, so packraven won't announce anything.{/if}
      </p>
    </div>
    <div class="actions">
      <button class="ghost settings" onclick={onsettings}>Discord settings</button>
      <button class:busy={loading} disabled={loading} onclick={loadReleases}>{loading ? 'Checking' : 'Refresh'}</button>
    </div>
  </header>

  {#if releasesError}<p class="failed" role="alert">{releasesError}</p>{/if}
  {#if discordError}<p class="failed" role="alert">{discordError}</p>{/if}

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
                    <dd><span class="chip {statusTone(curseforge.status)}">{curseforge.status}</span></dd>
                    <dt>Server pack</dt>
                    <dd>
                      <!-- A missing server pack waits on you, not on CurseForge's review. -->
                      <span class="chip {release.serverPackMissing ? 'fail' : statusTone(curseforge.serverStatus)}">{curseforge.serverStatus}</span>
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
                  <div class="discord">
                    <span class="muted">Not uploaded</span>
                    <span class="discord-actions">
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
              <div class="discord">
                <span class:muted={!release.announcedAt}>{discordState(release)}</span>
                <span class="discord-actions">
                  {#if hasWebhook}
                    <button class="ghost" class:busy={announcing === release.version} disabled={announcing !== null} onclick={() => announceNow(release)}>
                      {release.announcedAt ? 'Announce again' : approved(release) ? 'Announce now' : 'Announce before approval'}
                    </button>
                  {/if}
                  <button class="ghost" onclick={() => editInDiscohook(release.version)} aria-describedby="discohook-hint">Edit in Discohook</button>
                </span>
              </div>
            </td>
          </tr>
          {#if release.serverPackMissing}
            <tr class="follow-up">
              <td colspan={2 + (showCurseforge ? 1 : 0) + (showModrinth ? 1 : 0)}>
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
  .scroll {
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

  .discord {
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

  .modrinth {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 10px;
  }

  .discord-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
    margin-left: -8px;
  }

  .discord-actions button {
    color: var(--forest);
    white-space: nowrap;
  }
</style>
