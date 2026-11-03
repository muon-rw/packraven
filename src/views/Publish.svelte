<script lang="ts">
  import Notice from '../components/Notice.svelte'
  import { onMount, tick } from 'svelte'
  import { ask } from '@tauri-apps/plugin-dialog'
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener'
  import { api, errorText, type OverrideFileView, type Overview, type PackConfig, type PublishedTo, type UnfinishedRelease, type UpdateCheckerSetup, type UpdateType, type UploadNames } from '../lib/api'
  import { job, onJobEnd } from '../lib/jobs.svelte'
  import RenderedChangelog from '../components/RenderedChangelog.svelte'
  import JobLog from '../components/JobLog.svelte'
  import LinkedText from '../components/LinkedText.svelte'
  import Twisty from '../components/Twisty.svelte'
  import OverrideDecisions from '../components/OverrideDecisions.svelte'
  import ServerPackSteps from '../components/ServerPackSteps.svelte'
  import { nav, showInFiles } from '../lib/nav.svelte'
  import { asReleaseType, asUpdateType, curseforgeProblem, releaseTag, repoName, UPDATE_TYPES, updateTypeEffect } from '../lib/format'

  let {
    overview,
    onchange,
    onchanges,
    onsettings,
    onreleases,
  }: { overview: Overview | null; onchange: () => void; onchanges: () => void; onsettings: () => void; onreleases: () => void } = $props()

  let push = $state(true)
  // Which collapsed groups of files you opened in What we'll ship.
  let groupsOpen = $state<Record<string, boolean>>({})

  // We keep the authors, licenses and other versions of only the files that need a decision, so we
  // look them up as they appear.
  let lookedUp = ''
  let lookingUp = $state(false)
  $effect(() => {
    const unknown = [...(overview?.notOnCurseforge ?? []), ...(overview?.notOnModrinth ?? [])].filter((file) => !file.known || !file.versionsKnown).map((file) => file.path)
    const key = unknown.join('\n')
    if (!unknown.length || key === lookedUp) return
    lookedUp = key
    lookingUp = true
    api
      .lookUpOverrideCandidates()
      .then(onchange, () => {})
      .finally(() => (lookingUp = false))
  })

  let retrying = $state(false)

  async function retryModrinth() {
    retrying = true
    try {
      await api.resyncModList(true)
      onchange()
    } finally {
      retrying = false
    }
  }

  function settingsAt(anchor: string) {
    nav.anchor = anchor
    onsettings()
  }
  let problems: string[] = $state([])
  let exportWarnings: string[] = $state([])
  let checking = $state(true)
  // The first check fills in most of the view, so until it lands we say what's happening at the top.
  let checkedOnce = $state(false)
  let config = $state<PackConfig | null>(null)
  let changelog = $state<string | null>(null)
  let changelogError: string | null = $state(null)
  let hasWebhook = $state(false)
  let updateChecker = $state<UpdateCheckerSetup | null>(null)
  let updateType = $state<UpdateType | 'none'>('none')
  let checkerRepo = $derived(updateChecker?.kind === 'github' ? updateChecker.repo : null)
  let chosenUpdateType = $derived(checkerRepo && updateType !== 'none' ? updateType : null)
  // What GitHub says would stop packraven adding the release, checked whenever you choose to add one.
  let checkerProblem = $state<string | null>(null)
  $effect(() => {
    checkerProblem = null
    if (!chosenUpdateType) return
    let current = true
    api.updateCheckerProblem().then(
      (problem) => current && (checkerProblem = problem),
      () => {},
    )
    return () => {
      current = false
    }
  })
  let previewing = $state(false)
  let previewError = $state<string | null>(null)
  let error: string | null = $state(null)
  let published: { version: string; pushed: boolean; clientName: string } | null = $state(null)
  let commitMessage = $state('')
  const packs = [
    { side: 'client', title: 'Client pack' },
    { side: 'server', title: 'Server pack' },
  ] as const
  type Side = (typeof packs)[number]['side']
  const noOverrides = () => ({ client: { displayName: '', fileName: '' }, server: { displayName: '', fileName: '' } })
  let names = $state<{ client: UploadNames; server: UploadNames }>(noOverrides())
  let renaming = $state({ client: false, server: false })
  let defaultNames = $state<{ client: UploadNames; server: UploadNames } | null>(null)
  let identity = $state<string | null>(null)

  let toCurseforge = $derived(!!overview?.projectId)
  let toModrinth = $derived(!!overview?.modrinthProjectId)
  let platforms = $derived([toCurseforge && 'CurseForge', toModrinth && 'Modrinth'].filter(Boolean).join(' and ') || 'CurseForge')
  // Modrinth gets one mrpack, so the client and server pack names only apply on CurseForge.
  let shownPacks = $derived(toCurseforge || !toModrinth ? packs : [])
  let modrinthFile = $state('')
  let modrinthShipped = $state<OverrideFileView[]>([])
  let modrinthLeftOut = $state<OverrideFileView[]>([])
  let curseforgeShipped = $state<OverrideFileView[]>([])
  let curseforgeLeftOut = $state<OverrideFileView[]>([])
  let shippedByAuthor = $derived(
    [...new Set(modrinthShipped.flatMap((file) => (file.author ? [file.author] : [])))].map((author) => ({ author, files: modrinthShipped.filter((file) => file.author === author) })),
  )
  let shippedOpenLicense = $derived(modrinthShipped.filter((file) => !file.author && file.openLicense))
  let shippedOnYourWord = $derived(modrinthShipped.filter((file) => !file.author && !file.openLicense))
  // Which platforms the last publish reached, read back from what packraven recorded, so the notice
  // never names a platform packraven hasn't confirmed.
  let landed = $state<PublishedTo | 'pending' | 'unknown'>('pending')
  let landedOn = $derived(typeof landed === 'object' ? [landed.curseforge && 'CurseForge', landed.modrinth && 'Modrinth'].filter(Boolean).join(' and ') : '')

  // Releases that went out without their server pack or Modrinth pack, so you can finish them on any later visit.
  let unfinished = $state<UnfinishedRelease[]>([])
  // A retried server pack upload reports its own failure inline, so the job's failed notice would repeat it.
  let retriedServerPack = $state(false)
  // Once we know where a publish landed, its notice covers that release, even while you retry its server pack.
  let reported = $derived.by(() => (published && typeof landed === 'object' ? published.version : null))
  let earlierUnfinished = $derived(unfinished.filter((release) => release.version !== reported))
  // When the version you're on is a release packraven didn't finish, Publish finishes it instead of publishing it again.
  let resumable = $derived(unfinished.find((release) => release.version === trimmed) ?? null)
  // The release a Finish button is finishing, so its result gets a notice of its own.
  let finishing = $state<string | null>(null)
  let manualOpen = $state<Record<string, boolean>>({})

  function loadUnfinished() {
    return api.unfinishedReleases().then(
      (list) => (unfinished = list),
      () => (unfinished = []),
    )
  }

  let lastTag = $derived(overview?.lastTag ?? null)
  let lastVersion = $derived(overview?.lastVersion ?? null)

  onMount(() => {
    loadChangelog()
    loadUnfinished()
    api.getPackConfig().then(
      (c) => {
        config = c
        resetUpdateType()
      },
      () => (config = null),
    )
    api.updateCheckerSetup().then(
      (setup) => (updateChecker = setup),
      () => (updateChecker = null),
    )
    api.getWebhook().then((w) => (hasWebhook = !!w), () => (hasWebhook = false))
    api.getSettings().then(
      (s) => (identity = [s.gitName.trim(), s.gitEmail.trim() && `<${s.gitEmail.trim()}>`].filter(Boolean).join(' ') || null),
      () => (identity = null),
    )
  })

  function resetUpdateType() {
    updateType = config?.update_checker.add_releases ? config.update_checker.update_type : 'none'
  }

  onJobEnd('publish', (phase) => {
    loadUnfinished()
    if (phase !== 'done') return
    commitMessage = ''
    resetUpdateType()
    names = noOverrides()
    renaming = { client: false, server: false }
    loadChangelog()
    loadUnfinished()
    if (published) api.publishedTo(published.version).then((to) => (landed = to ?? 'unknown'), () => (landed = 'unknown'))
  })

  async function previewInDiscohook() {
    previewing = true
    try {
      openUrl(await api.discohookPreview(trimmed || 'next'))
      previewError = null
    } catch (e) {
      previewError = `Couldn't open Discohook: ${errorText(e)}`
    } finally {
      previewing = false
    }
  }

  async function loadChangelog() {
    try {
      changelog = (await api.changelogPreview()).markdown
      changelogError = null
    } catch (e) {
      changelogError = `Couldn't build the changelog preview: ${errorText(e)}`
    }
  }

  let trimmed = $derived(overview?.version?.trim() ?? '')
  let defaultMessage = $derived(`Release ${trimmed}`)
  let uncommitted = $derived(overview?.changed ?? [])

  $effect(() => {
    const wanted = trimmed
    void overview?.unhosted.length
    void overview?.notOnCurseforge.length
    void overview?.notOnModrinth.length
    void job('publish').finishedRuns
    let current = true
    checking = true
    const timer = setTimeout(() => {
      api.publishPreflight(wanted).then(
        (result) => {
          if (!current) return
          checkedOnce = true
          problems = result.problems
          exportWarnings = result.warnings
          tagTaken = result.tagTaken
          defaultNames = { client: result.client, server: result.server }
          modrinthFile = result.modrinthFile
          modrinthShipped = result.modrinthShipped
          modrinthLeftOut = result.modrinthLeftOut
          curseforgeShipped = result.curseforgeShipped
          curseforgeLeftOut = result.curseforgeLeftOut
        },
        (e) => {
          if (!current) return
          checkedOnce = true
          problems = [`Couldn't run the pre-publish checks: ${errorText(e)}`]
        },
      ).finally(() => current && (checking = false))
    }, 300)
    return () => {
      current = false
      clearTimeout(timer)
    }
  })

  const nameOf = (file: OverrideFileView) => file.name ?? file.path
  let unlisted = $derived(curseforgeShipped.filter((file) => !file.allowlisted))

  function describe(file: OverrideFileView): string {
    const name = file.name ?? file.path
    if (file.author) return `${name} (${file.author}'s permission)`
    if (file.allowlisted) return `${name} (on CurseForge's list)`
    return file.license ? `${name} (${file.license})` : name
  }

  function trimmedNames(side: Side): UploadNames {
    return { displayName: names[side].displayName.trim(), fileName: names[side].fileName.trim() }
  }

  function withVersion(template: string) {
    return trimmed ? template.replaceAll('{version}', trimmed) : template
  }

  function placeholder(side: Side, field: keyof UploadNames) {
    return withVersion(defaultNames?.[side][field] ?? '')
  }

  function resolvedNames(side: Side): UploadNames {
    const fileName = withVersion(names[side].fileName.trim() || (defaultNames?.[side].fileName ?? ''))
    return {
      displayName: withVersion(names[side].displayName.trim() || (defaultNames?.[side].displayName ?? '')),
      fileName: /\.zip$/i.test(fileName) ? fileName : `${fileName}.zip`,
    }
  }

  // The backend applies the same rules in upload_names; we repeat them so the field can show what's wrong as you type.
  function fileNameError(side: Side, title: string) {
    const name = names[side].fileName.trim()
    if (/[\\/:*?"<>|]/.test(name)) return `The ${title.toLowerCase()} file name can't contain any of / \\ : * ? " < > |.`
    if (/^\.zip$/i.test(name)) return `The ${title.toLowerCase()} file name needs a name before .zip.`
    return null
  }

  let fileNameErrors = $derived(Object.fromEntries(packs.map(({ side, title }) => [side, fileNameError(side, title)])) as Record<Side, string | null>)

  let staleVersionWarnings = $derived(
    lastVersion && lastVersion !== trimmed
      ? packs.flatMap(({ side, title }) =>
          (['displayName', 'fileName'] as const)
            .filter((field) => names[side][field].includes(lastVersion))
            .map(
              (field) =>
                `The ${title.toLowerCase()} ${field === 'displayName' ? 'display name' : 'file name'} still says ${lastVersion}. Write {version} instead so it follows the version you publish.`,
            ),
        )
      : [],
  )

  // You can't take back an upload, so we block a push that would fail after it.
  let blockers = $derived(
    [...problems, ...Object.values(fileNameErrors), overview?.behind && push ? `This branch is ${overview.behind} ${overview.behind === 1 ? 'commit' : 'commits'} behind its remote, so the push would fail after the upload. Pull first, or untick the push.` : null].filter(
      (p): p is string => !!p,
    ),
  )
  let tagTaken = $state(false)
  // A released version's file that waits on you on CurseForge, which Releases can fix. We look it up after the
  // checks load, and a slow or failed lookup just leaves the note out.
  let attention = $state<{ version: string; text: string } | null>(null)
  $effect(() => {
    const version = trimmed
    attention = null
    if (!tagTaken || !version) return
    let current = true
    api.releaseAttention(version).then(
      (found) => {
        const text = found && curseforgeProblem(found.state, version, found.file)
        if (current && text) attention = { version, text }
      },
      () => {},
    )
    return () => {
      current = false
    }
  })
  let emptyNotes = $derived(changelog !== null && !changelog.trim())
  let warnings = $derived(
    [
      emptyNotes ? `No release notes and no mod changes, so the changelog on ${platforms} will be empty.` : null,
      ...staleVersionWarnings,
      ...exportWarnings,
    ].filter((w): w is string => !!w),
  )

  let rolePing = $derived(!!config?.discord.role_id.trim())
  let announces = $derived(hasWebhook && !!config?.discord.announce_on_approval)
  let approvalWait = $derived(toCurseforge ? 'CurseForge approves it' : "it's on Modrinth")

  async function publish() {
    const client = resolvedNames('client')
    const server = resolvedNames('server')
    const asType = asReleaseType(config?.curseforge.release_type ?? 'release')
    const consequences = [
      toCurseforge
        ? `Upload "${client.displayName}" (${client.fileName}) and its server pack "${server.displayName}" (${server.fileName}), which downloads its mods on first start, to CurseForge as ${asType}${config?.curseforge.manual_release ? ', held until you release it' : ''}.`
        : null,
      toCurseforge && curseforgeShipped.length
        ? `Include ${curseforgeShipped.length === 1 ? 'a jar' : `${curseforgeShipped.length} jars`} CurseForge doesn't host inside the client pack: ${curseforgeShipped.map(describe).join(', ')}.`
        : null,
      toCurseforge && unlisted.length
        ? `We checked CurseForge's list of allowed mods and didn't find ${unlisted.length < curseforgeShipped.length ? unlisted.map(nameOf).join(', ') : curseforgeShipped.length === 1 ? 'it' : 'any of them'}, so CurseForge will reject the pack unless you have permission for ${unlisted.length === 1 ? 'it' : 'them'}.`
        : null,
      toModrinth ? `Upload "${client.displayName}" (${withVersion(modrinthFile)}) to Modrinth as ${asType}, listed right away.` : null,
      toModrinth && modrinthShipped.length
        ? `Include ${modrinthShipped.length === 1 ? 'a file' : `${modrinthShipped.length} files`} Modrinth doesn't host inside the Modrinth pack, on your word that you may share ${modrinthShipped.length === 1 ? 'it' : 'them'}: ${modrinthShipped.map(describe).join(', ')}.`
        : null,
      `Commit ${uncommitted.length ? `your ${uncommitted.length} uncommitted ${uncommitted.length === 1 ? 'file' : 'files'} and ` : ''}the release as "${commitMessage.trim() || defaultMessage}", tag it ${releaseTag(trimmed)}${push ? ', then push' : ''}.`,
      'Archive the release notes and clear changelog.md for the next release.',
      announces ? `Post to Discord once ${approvalWait}${rolePing ? ', pinging the role' : ''}.` : null,
      checkerRepo && chosenUpdateType ? `Add it to ${repoName(checkerRepo)} on GitHub as ${asUpdateType(chosenUpdateType)} once ${approvalWait}, so players see it in game.` : null,
      checkerRepo && !chosenUpdateType ? `Leave it out of ${repoName(checkerRepo)}, so players won't be told about it in game.` : null,
      emptyNotes ? `The changelog on ${platforms} will be empty.` : null,
    ].filter(Boolean)
    const confirmed = await ask(`${consequences.join('\n')}\n\nThis can't be undone.`, {
      title: `Publish ${trimmed}`,
      kind: 'warning',
      okLabel: `Publish ${trimmed}`,
      cancelLabel: 'Cancel',
    })
    if (!confirmed) return
    try {
      error = null
      retriedServerPack = false
      finishing = null
      landed = 'pending'
      published = { version: trimmed, pushed: push, clientName: client.displayName }
      await api.publish({
        version: trimmed,
        message: commitMessage.trim(),
        push,
        client: trimmedNames('client'),
        server: trimmedNames('server'),
        updateChecker: chosenUpdateType,
      })
      onchange()
    } catch (e) {
      error = errorText(e)
    }
  }

  // Recording a manual upload removes the control you used, so we move focus to what's left of that release, or to
  // the view's title once nothing is.
  async function markedUploaded(version: string) {
    await loadUnfinished()
    await tick()
    const target = document.getElementById(`unfinished-${version}`) ?? document.getElementById('publish-title')
    target?.focus()
  }

  async function finish(release: UnfinishedRelease) {
    const tagging = release.pending.find((step) => step.step === 'tag')
    const rebuild = !!release.serverPackMissing?.rebuild
    const steps = [
      release.serverPackMissing
        ? rebuild
          ? 'Build the server pack again from the pack as it is now, with its mods downloaded on first start, and upload it to CurseForge.'
          : 'Upload the server pack to CurseForge.'
        : null,
      release.modrinthMissing ? `Upload ${release.modrinthMissing} to Modrinth.` : null,
      tagging
        ? `Commit ${uncommitted.length ? `your ${uncommitted.length} uncommitted ${uncommitted.length === 1 ? 'file' : 'files'} and ` : ''}the release as "${tagging.message}", and tag it ${releaseTag(release.version)}.`
        : null,
      release.pending.some((step) => step.step === 'push') ? 'Push the release commit and tag.' : null,
    ].filter(Boolean)
    const mismatched = [tagging ? `its tag` : null, rebuild ? 'the rebuilt server pack' : null].filter(Boolean).join(' and ')
    const changed = release.packChanged && mismatched ? `\n\nYour mods changed since ${release.version} shipped, so ${mismatched} won't match what's already on CurseForge.` : ''
    const checked = rebuild && problems.length ? `\n\nPublish's checks found ${problems.length === 1 ? 'a problem' : `${problems.length} problems`} with the pack as it is now, and we build the server pack from it: ${problems.join(' ')}` : ''
    const confirmed = await ask(`${steps.join('\n')}${changed}${checked}`, {
      title: `Finish publishing ${release.version}`,
      kind: 'warning',
      okLabel: (release.packChanged && mismatched) || checked ? 'Finish anyway' : `Finish ${release.version}`,
      cancelLabel: 'Cancel',
    })
    if (!confirmed) return
    try {
      error = null
      published = null
      retriedServerPack = false
      finishing = release.version
      await api.finishRelease(release.version)
      onchange()
    } catch (e) {
      finishing = null
      error = `Couldn't finish ${release.version}: ${errorText(e)}`
    }
  }

  async function build(action: () => Promise<void>, what: string) {
    try {
      error = null
      await action()
    } catch (e) {
      error = `Couldn't start the ${what}: ${errorText(e)}`
    }
  }

  function reveal(path: string) {
    revealItemInDir(path).catch((e) => (error = `Couldn't open the folder: ${errorText(e)}`))
  }

  let publishJob = $derived(job('publish'))
  let publishing = $derived(publishJob.phase === 'running')
  let serverJob = $derived(job('server-pack'))
  let clientJob = $derived(job('client-export'))
  let modrinthJob = $derived(job('modrinth-export'))
  let builds = $derived([serverJob, clientJob, modrinthJob])
  let logLines = $derived(publishJob.lines.concat(...builds.map((b) => b.lines)).sort((a, b) => a.id - b.id))
  let showLog = $derived(publishing || builds.some((b) => b.phase === 'running') || logLines.length > 0)
  let projectUrl = $derived(overview?.projectId ? `https://www.curseforge.com/projects/${overview.projectId}` : null)
  let modrinthUrl = $derived(overview?.modrinthProjectId ? `https://modrinth.com/project/${encodeURIComponent(overview.modrinthProjectId)}` : null)
  let releaseType = $derived(
    config ? ({ release: 'Release', beta: 'Beta', alpha: 'Alpha' } as Record<string, string>)[config.curseforge.release_type] ?? config.curseforge.release_type : null,
  )
  let canPublish = $derived(!publishing && !checking && !blockers.length && !!trimmed)
</script>

<div class="publish">
  <header class="head">
    <div>
      <h2 id="publish-title" tabindex="-1">Publish</h2>
      <p class="muted">Review what you'll ship, then publish at the bottom. Publishing can't be undone.</p>
    </div>
    <div class="builds">
      <span class="muted small">Build without publishing</span>
      <span class="row-start">
        <button class:busy={serverJob.phase === 'running'} disabled={serverJob.phase === 'running'} onclick={() => build(api.buildServerPack, 'server pack build')}>
          {serverJob.phase === 'running' ? 'Building' : 'Server pack'}
        </button>
        <button class:busy={clientJob.phase === 'running'} disabled={clientJob.phase === 'running'} onclick={() => build(api.buildClientExport, 'client pack build')}>
          {clientJob.phase === 'running' ? 'Building' : 'Client pack'}
        </button>
        {#if toModrinth}
          <button class:busy={modrinthJob.phase === 'running'} disabled={modrinthJob.phase === 'running'} onclick={() => build(api.buildModrinthExport, 'Modrinth pack build')}>
            {modrinthJob.phase === 'running' ? 'Building' : 'Modrinth pack'}
          </button>
        {/if}
      </span>
      {#if builds.some((b) => b.phase === 'done' && b.message)}
        <span class="reveal">
          {#if serverJob.phase === 'done' && serverJob.message}<button class="link" onclick={() => reveal(serverJob.message!)}>Show server pack</button>{/if}
          {#if clientJob.phase === 'done' && clientJob.message}<button class="link" onclick={() => reveal(clientJob.message!)}>Show client pack</button>{/if}
          {#if modrinthJob.phase === 'done' && modrinthJob.message}<button class="link" onclick={() => reveal(modrinthJob.message!)}>Show Modrinth pack</button>{/if}
        </span>
      {/if}
    </div>
  </header>

  {#if checking && !checkedOnce}
    <p class="loading muted" role="status"><span class="spinner" aria-hidden="true"></span>Checking the pack before you publish</p>
  {/if}
  {#if published && (publishJob.phase === 'done' || reported)}
    {@const tagged = `packraven tagged the release commit ${releaseTag(published.version)}${published.pushed ? ' and pushed it' : " but hasn't pushed it yet"}`}
    {#if landed === 'pending'}
      <p class="muted" role="status">Confirming where {published.version} landed.</p>
    {:else if landed === 'unknown'}
      <Notice tone="warn">
        <p><strong>{overview?.name} {published.version} is published, but packraven couldn't read back where it landed.</strong> {tagged}.</p>
        <p><button onclick={onreleases}>Check it in Releases</button></p>
      </Notice>
    {:else}
      {@const modrinthFailed = toModrinth && landed.curseforge && !landed.modrinth}
      {#if landed.serverPackMissing}
        {@const headline = `${overview?.name} ${published.version} is on ${landedOn}, but its server pack isn't.`}
        <!-- The notice holds controls, so only its headline is announced. -->
        <p class="visually-hidden" role="status">{headline}</p>
        <Notice tone="warn" live={false}>
          <p><strong>{headline}</strong> {tagged}, and archived its notes to <span class="file">changelogs/{published.version}.md</span>.</p>
          <ServerPackSteps
            upload={landed.serverPackMissing}
            version={published.version}
            clientName={published.clientName}
            onretry={() => (retriedServerPack = true)}
            onmarked={() => published && api.publishedTo(published.version).then((to) => (landed = to ?? 'unknown'))}
          />
          {#if announces}<p class="muted small">packraven holds the Discord announcement until CurseForge has the server pack.</p>{/if}
        </Notice>
      {/if}
      {#if modrinthFailed}
        <Notice tone="warn">
          <p>
            <strong>{overview?.name} {published.version} is on CurseForge, but the Modrinth upload failed.</strong>
            {landed.serverPackMissing ? 'packraven' : `${tagged}, and`} kept the Modrinth pack it built.
          </p>
          {#if landed.modrinthError}<p class="muted"><LinkedText text={landed.modrinthError} /></p>{/if}
          <p><button onclick={onreleases}>Upload it from Releases</button></p>
        </Notice>
      {:else if !landed.serverPackMissing}
        <Notice tone="ok">
          <p><strong>{overview?.name} {published.version} is on {landedOn}.</strong> {tagged}.</p>
          <p class="muted">
            packraven archived its notes to <span class="file">changelogs/{published.version}.md</span> and cleared changelog.md for the next release. {publishJob.message}
            <button class="link" onclick={onreleases}>Follow its review in Releases</button>.
          </p>
        </Notice>
      {/if}
    {/if}
  {/if}
  {#each earlierUnfinished as release (release.version)}
    {@const name = release.displayName ?? `${overview?.name} ${release.version}`}
    {@const tag = releaseTag(release.version)}
    {@const tagging = release.pending.some((step) => step.step === 'tag')}
    {@const pushing = release.pending.some((step) => step.step === 'push')}
    {@const rebuilding = !!release.serverPackMissing?.rebuild}
    <!-- The notice holds controls, so it isn't a live region; its heading lets you jump between unfinished releases. -->
    <Notice tone="warn" live={false}>
      <h3 class="notice-title" id="unfinished-{release.version}" tabindex="-1">{name} is out, but we didn't finish publishing it</h3>
      <ul class="remaining" aria-label="Left to do for {release.version}">
        {#if release.serverPackMissing}
          <li>
            Upload the server pack to CurseForge{rebuilding ? ', after building it again with its mods downloaded on first start' : ''}.
            {#if release.serverPackMissing.error}<span class="muted"><LinkedText text={`${release.serverPackMissing.error.trim().replace(/[.!]+$/, '')}.`} /></span>{/if}
          </li>
        {/if}
        {#if release.modrinthMissing}
          <li>
            Upload <span class="file">{release.modrinthMissing}</span> to Modrinth.
            {#if release.modrinthError}<span class="muted">The last try failed: <LinkedText text={`${release.modrinthError.trim().replace(/[.!]+$/, '')}.`} /></span>{/if}
          </li>
        {/if}
        {#if tagging}<li>Commit the release and tag it <span class="file">{tag}</span>{pushing ? ', then push' : ''}.</li>{:else if pushing}<li>Push <span class="file">{tag}</span>.</li>{/if}
      </ul>
      {#if release.packChanged && (tagging || rebuilding)}
        <p class="muted">
          Your mods changed since {release.version} shipped. If you finish now, {[tagging ? 'the tag' : null, rebuilding ? 'the rebuilt server pack' : null].filter(Boolean).join(' and ')}
          will record the pack as it is now, not the one on CurseForge.
        </p>
      {/if}
      <!-- When you're on this version, the button at the bottom of Publish finishes it, so we don't offer a second one. -->
      {#if release.version === trimmed}
        <p class="muted">Finish it with the button at the bottom of Publish.</p>
      {:else}
        <p><button class="primary" class:busy={publishing && finishing === release.version} disabled={publishing} onclick={() => finish(release)}>Finish publishing {release.version}</button></p>
      {/if}
      {#if release.serverPackMissing}
        <details class="manual" bind:open={manualOpen[release.version]}>
          <summary class="muted small"><Twisty open={!!manualOpen[release.version]} />Upload the server pack yourself</summary>
          <ServerPackSteps upload={release.serverPackMissing} version={release.version} clientName={name} retry={false} tagged={!tagging} onmarked={() => markedUploaded(release.version)} />
        </details>
      {/if}
    </Notice>
  {/each}
  {#if publishJob.phase === 'done' && finishing && publishJob.message}
    <Notice tone="ok"><p>{publishJob.message}. CurseForge links a new server pack once the client pack is public, so Releases shows it then.</p></Notice>
  {/if}
  <!-- A retried upload from a notice above runs as a publish job too, with no publish of its own to report it. -->
  {#if publishJob.phase === 'done' && retriedServerPack && !published && publishJob.message}
    <Notice tone="ok"><p>{publishJob.message}.</p></Notice>
  {/if}
  {#each [publishJob, ...builds] as failed, index (index)}
    {#if failed.phase === 'failed' && !(index === 0 && retriedServerPack)}<Notice tone="error"><p><LinkedText text={failed.message ?? ''} /></p></Notice>{/if}
  {/each}
  {#if error}<Notice tone="error"><p><LinkedText text={error} /></p></Notice>{/if}

  <section class="step" aria-labelledby="version-title">
    <h3 id="version-title">Version</h3>
    <div class="version-row">
      <span class="version" class:taken={tagTaken}>{trimmed || 'Not set'}</span>
      {#if trimmed}
        <button class="link" aria-label="Change the version" onclick={() => (nav.editVersion = true)}>Change</button>
      {:else}
        <button class="link" onclick={onchanges}>Set up Better Compatibility Checker in Changes</button>
      {/if}
    </div>
    <p class="muted small">
      {#if lastTag}
        The last release is <span class="file">{lastTag}</span>.
      {:else}
        This is the first release.
      {/if}
    </p>
  </section>

  <section class="step" aria-labelledby="checks-title" aria-busy={checking}>
    <div class="checks-head">
      <h3 id="checks-title">Checks</h3>
      {#if checking}<span class="loading muted small" role="status"><span class="spinner" aria-hidden="true"></span>Checking</span>{/if}
    </div>
    <!-- We keep the last results in view while a recheck runs, dimmed so they don't read as current. -->
    <ul class="checks" id="publish-checks" class:stale={checking && checkedOnce}>
      {#each blockers as problem (problem)}
        <li class="bad">
          <svg viewBox="0 0 16 16" role="img" aria-label="Blocks publishing"><path d="M4 4l8 8M12 4l-8 8" /></svg>
          <span><LinkedText text={problem} /></span>
        </li>
      {/each}
      {#if attention && attention.version === trimmed}
        <li class="warn">
          <svg viewBox="0 0 16 16" role="img" aria-label="Warning"><path d="M8 3v6M8 12v1" /></svg>
          <span>{attention.text}. <button class="link" onclick={onreleases}>See it in Releases</button></span>
        </li>
      {/if}
      {#each warnings as warning (warning)}
        <li class="warn">
          <svg viewBox="0 0 16 16" role="img" aria-label="Warning"><path d="M8 3v6M8 12v1" /></svg>
          {warning}
        </li>
      {/each}
      {#if checkerProblem}
        <li class="warn">
          <svg viewBox="0 0 16 16" role="img" aria-label="Warning"><path d="M8 3v6M8 12v1" /></svg>
          <span>packraven won't be able to add {trimmed || 'this release'} to the update checker. {checkerProblem} <button class="link" onclick={() => (nav.view = 'settings')}>Open App settings</button></span>
        </li>
      {/if}
      {#if !blockers.length && !warnings.length && !checkerProblem && !checking}
        <li class="ok">
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 8.5l3 3 7-7" /></svg>
          Ready to publish
        </li>
      {/if}
    </ul>
    {#if overview?.modrinthError}
      <Notice tone="warn">
        <p>
          Couldn't check which files Modrinth hosts, so you can't publish to Modrinth until packraven can. <LinkedText text={overview.modrinthError} />
        </p>
        <p><button class:busy={retrying} disabled={retrying} onclick={retryModrinth}>{retrying ? 'Checking' : 'Check again'}</button></p>
      </Notice>
    {/if}
    {#if overview}
      {@const dual = overview.projectId > 0 && !!overview.modrinthProjectId}
      <OverrideDecisions platform="curseforge" candidates={overview.notOnCurseforge} {dual} checking={lookingUp} {onchange} onsettings={() => settingsAt('curseforge-choices-title')} />
      <OverrideDecisions platform="modrinth" candidates={overview.notOnModrinth} {dual} checking={lookingUp} {onchange} onsettings={() => settingsAt('modrinth-choices-title')} />
    {/if}
    {#if overview?.devJars.length}
      <Notice tone="warn">
        <p>Local dev builds are in the mods folder and won't be part of the release: <span class="file">{overview.devJars.join(', ')}</span></p>
      </Notice>
    {/if}
  </section>

  <section class="step" aria-labelledby="ships-title">
    <h3 id="ships-title">What we'll ship</h3>
    <dl class="ships">
      {#each shownPacks as { side, title } (side)}
        {@const resolved = resolvedNames(side)}
        {@const typed = trimmedNames(side)}
        {@const fileError = fileNameErrors[side]}
        {@const open = renaming[side] || !!fileError}
        <div>
          <dt>{title}</dt>
          <dd class="pack-names">
            <button type="button" class="summary" aria-expanded={open} aria-controls="{side}-rename" onclick={() => (renaming[side] = !open)}>
              <span class="resolved">
                {#if defaultNames}
                  <span>{resolved.displayName}</span>
                  <span class="file muted">{resolved.fileName}</span>
                {:else}
                  <span class="muted">Checking</span>
                {/if}
              </span>
              <span class="rename"><Twisty {open} />Rename<span class="visually-hidden"> the {title.toLowerCase()}</span></span>
            </button>
            {#if side === 'server'}<span class="muted small">We upload it as an additional file of the client pack.</span>{/if}
            {#if open}
              <div class="names" id="{side}-rename">
                <label for="{side}-display-name"><span class="visually-hidden">{title} </span>Display name</label>
                <input
                  id="{side}-display-name"
                  bind:value={names[side].displayName}
                  placeholder={placeholder(side, 'displayName')}
                  aria-describedby="{side}-display-name-note {side}-names-hint"
                />
                {#if typed.displayName}
                  <span class="field-note muted small" id="{side}-display-name-note">
                    {#if trimmed && resolved.displayName !== typed.displayName}Shows on CurseForge as {resolved.displayName}.{/if}
                    <button type="button" class="link" onclick={() => (names[side].displayName = '')}>Use the default</button>
                  </span>
                {/if}
                <label for="{side}-file-name"><span class="visually-hidden">{title} </span>File name</label>
                <input
                  id="{side}-file-name"
                  class="mono"
                  bind:value={names[side].fileName}
                  placeholder={placeholder(side, 'fileName')}
                  spellcheck="false"
                  aria-invalid={!!fileError}
                  aria-describedby="{side}-file-name-note {side}-names-hint"
                />
                {#if fileError}
                  <span class="field-note failed small" id="{side}-file-name-note">{fileError}</span>
                {:else if typed.fileName}
                  <span class="field-note muted small" id="{side}-file-name-note">
                    {#if trimmed && resolved.fileName !== typed.fileName}Uploads as <span class="file">{resolved.fileName}</span>.{/if}
                    <button type="button" class="link" onclick={() => (names[side].fileName = '')}>Use the default</button>
                  </span>
                {/if}
                <span class="note muted small" id="{side}-names-hint">
                  Leave a field empty to use the default shown. <span class="file">{'{version}'}</span> becomes the version you publish, and we add .zip to file names that
                  don't end in it.
                </span>
              </div>
            {/if}
          </dd>
        </div>
      {/each}
      {#if toCurseforge && (curseforgeShipped.length || curseforgeLeftOut.length)}
        <div>
          <dt>Jars CurseForge doesn't host</dt>
          <dd class="pack-names">
            {#if curseforgeShipped.length}
              {@const one = curseforgeShipped.length === 1}
              {@render fileGroup(
                'curseforge-included',
                one ? "We'll include the jar itself in the client pack" : "We'll include the jars themselves in the client pack",
                curseforgeShipped,
                describe,
                unlisted.length
                  ? `We checked CurseForge's list of allowed mods and didn't find ${one ? 'this one' : unlisted.length === curseforgeShipped.length ? 'these' : `${unlisted.length} of these`}, so CurseForge will reject the pack unless you have permission for ${unlisted.length === 1 ? 'it' : 'them'}.`
                  : null,
              )}
            {/if}
            {#if curseforgeLeftOut.length}
              {@render fileGroup('curseforge-left-out', `Left out of the CurseForge pack${toModrinth ? ', but still in the Modrinth pack' : ''}`, curseforgeLeftOut, nameOf)}
            {/if}
            <button class="link small" onclick={onsettings}>Change in Pack settings</button>
          </dd>
        </div>
      {/if}
      {#if toModrinth}
        <div>
          <dt>Modrinth pack</dt>
          <dd class="pack-names">
            <span class="resolved">
              {#if defaultNames}
                <span>{resolvedNames('client').displayName}</span>
                <span class="file muted">{withVersion(modrinthFile)}</span>
              {:else}
                <span class="muted">Checking</span>
              {/if}
            </span>
            <span class="muted small">
              One shared file for clients and servers.
            </span>
            <!-- Without the "We'll include" summary above it, a lone reason has to say the files are included. -->
            {#snippet modrinthIncludes(alone: boolean)}
              {#each shippedByAuthor as group (group.author)}
                {@render fileGroup(`modrinth-author-${group.author}`, `${alone ? 'Included, on' : 'On'} ${group.author}'s permission`, group.files, nameOf)}
              {/each}
              {#if shippedOpenLicense.length}
                {@render fileGroup('modrinth-open-license', alone ? 'Included, under an open license' : 'Under an open license', shippedOpenLicense, describe)}
              {/if}
              {#if shippedOnYourWord.length}
                {@render fileGroup('modrinth-word', alone ? 'Included, on your word that you may share' : 'On your word that you may share', shippedOnYourWord, describe)}
              {/if}
            {/snippet}
            <!-- Several reasons fold under one line; a single reason is its own group. -->
            {#if shippedByAuthor.length + (shippedOpenLicense.length ? 1 : 0) + (shippedOnYourWord.length ? 1 : 0) > 1}
              <details class="file-group" bind:open={groupsOpen['modrinth-included']}>
                <summary class="muted small">
                  <Twisty open={!!groupsOpen['modrinth-included']} />We'll include files Modrinth doesn't host ({modrinthShipped.length} files)
                </summary>
                <div class="group-body">{@render modrinthIncludes(false)}</div>
              </details>
            {:else if modrinthShipped.length}
              {@render modrinthIncludes(true)}
            {/if}
            {#if modrinthLeftOut.length}
              {@render fileGroup('modrinth-left-out', 'Left out, but still in the CurseForge pack', modrinthLeftOut, nameOf)}
            {/if}
            {#if modrinthShipped.length || modrinthLeftOut.length}<button class="link small" onclick={onsettings}>Change in Pack settings</button>{/if}
          </dd>
        </div>
      {/if}
      <div>
        <dt>Release type</dt>
        <dd>
          {releaseType ?? 'Unknown'}{config?.curseforge.manual_release ? ', held until you release it' : ''}
          <button class="link" onclick={onsettings}>Change</button>
        </dd>
      </div>
      <div>
        <dt>{toCurseforge && toModrinth ? 'Projects' : 'Project'}</dt>
        <dd class="projects">
          {#if projectUrl}<span>CurseForge <a href={projectUrl} onclick={(e) => { e.preventDefault(); openUrl(projectUrl!) }}>{overview?.projectId}</a></span>{/if}
          {#if modrinthUrl}<span>Modrinth <a href={modrinthUrl} onclick={(e) => { e.preventDefault(); openUrl(modrinthUrl!) }}>{overview?.modrinthProjectId}</a></span>{/if}
          {#if !projectUrl && !modrinthUrl}Not set{/if}
        </dd>
      </div>
      <div>
        <dt>Git</dt>
        <dd>{#if trimmed}Commit and tag <span class="file">{releaseTag(trimmed)}</span>{push ? ', then push' : ''}{:else}<span class="muted">Set a version first</span>{/if}</dd>
      </div>
      <div>
        <dt>Notes</dt>
        <dd>
          {#if trimmed}We'll archive them to <span class="file">changelogs/{trimmed}.md</span>, then clear changelog.md{:else}<span class="muted">Set a version first</span>{/if}
        </dd>
      </div>
    </dl>
  </section>

  <section class="step" aria-labelledby="commit-title">
    <h3 id="commit-title">Release commit</h3>
    <label class="commit-field">
      <span class="muted small">Commit message</span>
      <input bind:value={commitMessage} placeholder={defaultMessage} aria-describedby="commit-hint" />
    </label>
    <p class="muted small" id="commit-hint">
      Leave it empty to use "{defaultMessage}". We'll commit any remaining changes along with the new version
      and archived notes. The commit author will be {identity ?? 'the name and email in your git config'}.
      <button class="link" onclick={() => (nav.view = 'settings')}>Change in App settings</button>
    </p>
    {#if uncommitted.length}
      <details>
        <summary>{uncommitted.length === 1 ? '1 uncommitted file' : `${uncommitted.length} uncommitted files`} will go into the release commit</summary>
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <ul class="uncommitted" tabindex="0" aria-label="Uncommitted files">
          {#each uncommitted as path (path)}<li class="file">{path}</li>{/each}
        </ul>
      </details>
    {/if}
  </section>

  <section class="step" aria-labelledby="changelog-title">
    <div class="row">
      <h3 id="changelog-title">Changelog on {platforms}</h3>
      <span class="row-start">
        <button class="ghost" onclick={onchanges}>Edit notes in Changes</button>
        <button class="ghost" onclick={loadChangelog}>Refresh</button>
      </span>
    </div>
    {#if changelogError}
      <Notice tone="error"><p>{changelogError}</p></Notice>
    {:else if changelog === null}
      <p class="muted">Building the preview.</p>
    {:else if !changelog.trim()}
      <div class="empty-changelog">
        <p>No release notes and no mod changes yet, so the changelog would be empty.</p>
        <p><button onclick={onchanges}>Write notes in Changes</button></p>
      </div>
    {:else}
      <RenderedChangelog markdown={changelog} label="Changelog preview" />
    {/if}
  </section>

  <section class="step" aria-labelledby="discord-title">
    <h3 id="discord-title">Discord</h3>
    <p class="muted">
      {#if !hasWebhook}
        No webhook set, so packraven won't announce this release.
      {:else if announces}
        packraven posts to Discord once {toCurseforge ? `CurseForge approves ${trimmed || 'the release'}` : `${trimmed || 'the release'} is on Modrinth`}{rolePing ? ', and pings the role' : ''}.
      {:else}
        Automatic announcements are off. You can announce it from Releases.
      {/if}
      <button class="link" onclick={onsettings}>{hasWebhook ? 'Change Discord settings' : 'Set up a webhook'}</button>
    </p>
    <div class="row-start">
      <button class:busy={previewing} disabled={previewing} onclick={previewInDiscohook}>{previewing ? 'Opening' : 'Preview the announcement'}</button>
    </div>
    {#if previewError}<p class="failed">{previewError}</p>{/if}
  </section>

  {#if updateChecker}
    <section class="step" aria-labelledby="update-checker-title">
      <h3 id="update-checker-title">Modpack Update Checker</h3>
      {#if updateChecker.kind === 'github'}
        {@const repo = updateChecker.repo}
        <label class="update-type">
          <span class="muted small">Update type for {trimmed || 'this release'}</span>
          <select bind:value={updateType} aria-describedby="update-checker-hint">
            {#each UPDATE_TYPES as { type, label } (type)}<option value={type}>{label} update</option>{/each}
            <option value="none">Don't add it</option>
          </select>
        </label>
        <div class="muted small" id="update-checker-hint">
          {#if chosenUpdateType}
            <p>{updateTypeEffect(chosenUpdateType, updateChecker.forceCompatible)}</p>
            <p>We'll only add it to {repoName(repo)} once {approvalWait}.</p>
          {:else}
            <p>packraven leaves it out of {repoName(repo)}, so players won't be told about it in game. You can still add it from Releases.</p>
          {/if}
          <p><button class="link" onclick={() => settingsAt('update-checker-title')}>Change the default in Pack settings</button></p>
        </div>
      {:else}
        {@const configPath = updateChecker.configPath}
        <p class="muted">{updateChecker.reason} Until then, add this release to the update checker yourself.</p>
        <p><button class="link" onclick={() => showInFiles(configPath)}>Open the config in Files</button></p>
      {/if}
    </section>
  {/if}

  {#if showLog}
    <section class="step log-block" aria-labelledby="log-title">
      <h3 id="log-title">Build log</h3>
      <JobLog lines={logLines} label="Build and publish log" />
    </section>
  {/if}

  <div class="go">
    {#if resumable}
      {@const release = resumable}
      <button class="primary" class:busy={publishing} disabled={publishing} onclick={() => finish(release)}>
        {publishing ? 'Finishing' : `Finish publishing ${trimmed}`}
      </button>
    {:else}
      <label class="check"><input type="checkbox" bind:checked={push} /> Push the release commit and tag</label>
      <button class="primary" class:busy={publishing} disabled={!canPublish} aria-describedby="publish-checks" onclick={publish}>
        {publishing ? 'Publishing' : `Publish ${trimmed || 'release'}`}
      </button>
    {/if}
  </div>
</div>

<!-- A group of more than one file collapses to a line with its count, so a long list doesn't push the
     rest of What we'll ship out of view. -->
{#snippet fileGroup(id: string, label: string, files: OverrideFileView[], describeFile: (file: OverrideFileView) => string, note: string | null = null)}
  {#if files.length === 1}
    <span class="muted small">{label}: <span class="file">{describeFile(files[0])}</span>{note ? `. ${note}` : ''}</span>
  {:else}
    <details class="file-group" bind:open={groupsOpen[id]}>
      <summary class="muted small"><Twisty open={!!groupsOpen[id]} />{label} ({files.length} files)</summary>
      <div class="group-body">
        {#if note}<span class="muted small">{note}</span>{/if}
        <ul class="small">
          {#each files as file (file.path)}<li>{describeFile(file)}</li>{/each}
        </ul>
      </div>
    </details>
  {/if}
{/snippet}

<style>
  .file-group {
    display: grid;
    justify-items: end;
  }

  .file-group summary {
    display: flex;
    align-items: center;
    gap: 4px;
    list-style: none;
    cursor: pointer;
  }

  .file-group summary::-webkit-details-marker {
    display: none;
  }

  .file-group summary:hover {
    color: var(--text);
  }

  .group-body {
    display: grid;
    justify-items: end;
    gap: 4px;
    margin-top: 4px;
  }

  .group-body ul {
    margin: 0;
    padding: 0;
    list-style: none;
    text-align: right;
  }

  /* Wide enough for a file decision's text and its buttons to share a row. Prose keeps its own measure. */
  .publish {
    display: grid;
    gap: 28px;
    max-width: 1240px;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: end;
    gap: 16px 24px;
  }

  .head p {
    margin-top: 4px;
  }

  .builds {
    display: grid;
    justify-items: end;
    gap: 6px;
    margin-left: auto;
  }

  .small {
    font-size: var(--step--1);
  }

  .step {
    display: grid;
    gap: 12px;
    align-content: start;
  }

  .version-row {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 10px 16px;
  }

  .version {
    font-family: var(--font-log);
    font-size: var(--step-1);
  }

  .version.taken {
    color: var(--fail);
  }

  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
  }

  .step {
    --changelog-max-height: 60vh;
  }

  .commit-field,
  .update-type {
    display: grid;
    gap: 4px;
    max-width: 480px;
  }

  #update-checker-hint {
    display: grid;
    gap: 4px;
    max-width: 60ch;
  }

  #commit-hint {
    max-width: 52ch;
  }

  summary {
    cursor: pointer;
  }

  .uncommitted li {
    overflow-wrap: anywhere;
  }

  .uncommitted {
    display: grid;
    gap: 2px;
    max-height: 200px;
    overflow: auto;
    margin: 6px 0 0;
    padding: 0;
    list-style: none;
    font-size: var(--step--1);
  }

  .empty-changelog {
    display: grid;
    gap: 12px;
    padding: 18px 22px;
    color: var(--muted);
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--radius-panel);
  }

  .ships {
    display: grid;
    margin: 0;
    border-top: 1px solid var(--line);
  }

  .ships > div {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: baseline;
    gap: 4px 16px;
    padding: 8px 0;
    border-bottom: 1px solid var(--line);
  }

  .ships dt {
    color: var(--muted);
    font-size: var(--step--1);
  }

  .ships dd {
    margin: 0;
    text-align: right;
  }

  .ships .projects {
    display: grid;
    justify-items: end;
    gap: 2px;
  }

  /* A fixed basis keeps both rows' columns aligned once they wrap below their labels. */
  .ships .names {
    flex: 0 1 480px;
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    align-items: center;
    gap: 6px 10px;
    text-align: left;
  }

  .ships .pack-names {
    flex: 0 1 480px;
    display: grid;
    justify-items: end;
    gap: 4px;
  }

  /* The negative margin lines the names up with the other rows while the hover area extends past them. */
  .pack-names .summary {
    display: flex;
    flex-wrap: wrap;
    justify-content: end;
    align-items: baseline;
    gap: 4px 12px;
    margin: -4px -8px;
    padding: 4px 8px;
    font-weight: 400;
    text-align: right;
    background: transparent;
    border-color: transparent;
  }

  .pack-names .summary:hover {
    background: var(--control);
  }

  .pack-names .summary:hover .rename {
    color: var(--forest-hi);
  }

  .pack-names .resolved {
    display: grid;
    justify-items: end;
    gap: 2px;
  }

  .pack-names .rename {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    color: var(--forest);
    font-size: var(--step--1);
    text-decoration: underline;
    text-underline-offset: 0.18em;
  }

  .pack-names .names {
    justify-self: stretch;
    margin-top: 6px;
  }

  .names label {
    justify-self: end;
  }

  .names .mono {
    font-family: var(--font-log);
  }

  .names input[aria-invalid='true'] {
    border-color: var(--fail-border);
  }

  .names .field-note {
    grid-column: 2;
  }

  .names .field-note .link {
    margin-left: 0;
  }

  .names .note {
    grid-column: 1 / -1;
  }

  .ships .link {
    margin-left: 8px;
    font-size: var(--step--1);
  }

  .loading {
    display: flex;
    align-items: center;
    margin: 0;
  }

  .checks-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 12px;
  }

  .checks.stale {
    opacity: 0.6;
  }

  .checks {
    display: grid;
    gap: 8px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .checks li {
    display: grid;
    grid-template-columns: 16px 1fr;
    gap: 10px;
    align-items: start;
  }

  .checks svg {
    width: 16px;
    height: 16px;
    margin-top: 3px;
    fill: none;
    stroke-width: 2.2;
    stroke-linecap: round;
    stroke-linejoin: round;
  }

  .checks .bad svg {
    stroke: var(--fail-icon);
  }

  .checks .warn svg {
    stroke: var(--warn-icon);
  }

  .checks .ok svg {
    stroke: var(--forest);
  }

  .log-block {
    grid-template-rows: auto minmax(240px, 40vh);
  }

  .row-start {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .reveal {
    display: flex;
    gap: 16px;
    font-size: var(--step--1);
  }

  /* We keep the button in reach while you read the review above it. */
  .go {
    position: sticky;
    bottom: -28px;
    display: flex;
    flex-wrap: wrap;
    justify-content: end;
    align-items: center;
    gap: 12px 20px;
    margin-bottom: -28px;
    padding: 16px 0 28px;
    background: var(--paper);
    border-top: 1px solid var(--line-strong);
  }

  .go .primary {
    padding: 10px 22px;
    font-size: var(--step-1);
  }

  .go .check {
    color: var(--muted);
    font-size: var(--step--1);
  }
  /* An unfinished release's notice: what's left as a short list, then one way to finish it. */
  .notice-title {
    font-size: var(--step-0);
    font-weight: 700;
  }

  .remaining {
    display: grid;
    gap: 6px;
    margin: 0;
    padding-left: 20px;
    max-width: 72ch;
  }

  .remaining .muted {
    display: block;
    font-size: var(--step--1);
  }

  .manual summary {
    display: flex;
    align-items: center;
    gap: 4px;
    width: fit-content;
    list-style: none;
    cursor: pointer;
  }

  .manual summary::-webkit-details-marker {
    display: none;
  }

  .manual summary:hover {
    color: var(--text);
  }

  .manual[open] summary {
    margin-bottom: 8px;
  }
</style>
