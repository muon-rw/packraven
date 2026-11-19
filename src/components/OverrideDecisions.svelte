<script lang="ts">
  import { tick } from 'svelte'
  import { openUrl } from '@tauri-apps/plugin-opener'
  import Notice from './Notice.svelte'
  import Tip from './Tip.svelte'
  import { api, errorText, type OverrideCandidate, type OverrideChoice, type Platform, type Skew } from '../lib/api'
  import { notOnHostsSummary } from '../lib/modsync'
  import { changeFor, changeQueued, dropChange, startChange } from '../lib/modUpdates.svelte'

  let {
    platform,
    candidates,
    dual,
    checking,
    onchange,
    onsettings,
  }: {
    platform: Platform
    candidates: OverrideCandidate[]
    // The pack goes to both platforms, so these files stay in the other one's pack either way.
    dual: boolean
    // We're looking up the files' authors, licenses and other versions.
    checking: boolean
    onchange: () => void
    onsettings: () => void
  } = $props()

  const PLATFORM = {
    modrinth: { name: 'Modrinth', other: 'CurseForge', guide: 'https://support.modrinth.com/en/articles/8797527-obtaining-modpack-permissions', guideLabel: "Modrinth's guide to modpack permissions", how: 'How Modrinth checks permissions' },
    curseforge: { name: 'CurseForge', other: 'Modrinth', guide: 'https://docs.google.com/spreadsheets/d/176Wv-PZUo9hFxy6oC6N8tWdquBLPRtSuLbNK-r0_byM/htmlview', guideLabel: "CurseForge's list of allowed mods", how: 'How CurseForge checks jars' },
  } as const
  // How we linked the two projects. It says nothing about whether their files match, so the copy
  // keeps it apart from what we say about builds.
  const EVIDENCE: Record<Skew['link']['evidence'], string> = { sameFiles: 'they share file names', sameSource: 'they share a source repository', sameAuthor: 'they share an author' }
  const DIFFERENCES_SHOWN = 12
  // Past a handful of rows, the notice would push the release notes out of view.
  const ROWS = 4
  // Long enough to catch the second press of a double click on a row that just moved up.
  const SETTLE_MS = 400

  type Undo = { kind: 'files'; paths: string[]; restore?: { path: string; choice: OverrideChoice } } | { kind: 'author'; author: string } | { kind: 'build'; path: string }
  type SkewAction = { kind: 'build' | 'switch'; version: string; label: string; primary: boolean; tip: string; fileId?: number }
  // After a choice about one file, the result offers the same choice for its other versions.
  type Widen = { path: string; pattern: string; choice: OverrideChoice; earlier?: { path: string; choice: OverrideChoice } | null }
  let copy = $derived(PLATFORM[platform])
  let showAll = $state(false)
  let choosing = $state<string | null>(null)
  let settling = $state(false)
  let result = $state<{ ok: boolean; text: string; undo?: Undo; widen?: Widen } | null>(null)
  // Where your place goes once the list catches up: the row that moved up after you include one, the
  // same button after you leave one out, or the result after a choice about several files.
  let refocus = $state<{ paths: string[]; index: number; target: 'row' | 'leaveOut' | 'result' } | null>(null)
  let permittingAuthor = $state('')

  let shown = $derived(showAll ? candidates : candidates.slice(0, ROWS))
  let paths = $derived(candidates.map((file) => file.path))
  // Each way to decide several files at once rests on a reason you can check: a license, an author's
  // word, or CurseForge's own list. Without one, including a file rests on your word alone.
  const isVouched = (file: OverrideCandidate) => (platform === 'modrinth' ? file.openLicense : file.allowlisted)
  let vouched = $derived(candidates.filter(isVouched))
  let authors = $derived(platform === 'modrinth' ? [...new Set(candidates.flatMap((file) => file.authors))].sort((a, b) => a.localeCompare(b)) : [])
  let busy = $derived(!!choosing || settling)
  const filesBy = (author: string) => candidates.filter((file) => file.authors.includes(author)).map((file) => file.path)
  const id = (part: string) => `${platform}-${part}`

  // Platforms name jars and zips differently in their rules, so the button says which it is.
  const fileKind = (path: string) => (path.endsWith('.jar') ? 'jar' : path.endsWith('.zip') ? 'zip' : 'file')

  function includeTip(path: string) {
    const what = fileKind(path) === 'file' ? 'this file' : `this ${fileKind(path)}`
    const rule =
      platform === 'modrinth'
        ? "Modrinth's moderators may ask you for proof that you may share it."
        : "CurseForge automatically rejects the pack unless this mod is on its list of allowed mods, or CurseForge gave you permission to include it."
    return `Bundle ${what} inside the ${copy.name} pack itself, so players get it even though ${copy.name} doesn't host it. ${rule}`
  }

  let leaveOutTip = $derived(
    `Leave this file out of the ${copy.name} pack. Use if you plan to tell players to download it themselves, for example with an in-game notice.${dual ? ` It stays in the ${copy.other} pack.` : ''}`,
  )

  async function showMore() {
    showAll = true
    await tick()
    document.getElementById(id(`row-${ROWS}`))?.focus()
  }

  async function choose(chosen: string[], choice: OverrideChoice, index: number, label: string, target: 'row' | 'leaveOut' | 'result') {
    const them = chosen.length === 1 ? 'it' : 'them'
    const text =
      choice === 'ship'
        ? `We'll include ${label} in the ${copy.name} pack.`
        : `We'll leave ${label} out of the ${copy.name} pack, so players who install that pack won't get ${them}.${dual ? ` ${them === 'it' ? 'It stays' : 'They stay'} in the ${copy.other} pack.` : ''}`
    const single = chosen.length === 1 ? candidates.find((file) => file.path === chosen[0]) : undefined
    const widen = single?.anyVersion ? { path: chosen[0], pattern: single.anyVersion, choice, earlier: single.earlier } : undefined
    await save(chosen, choice, `${choice}:${chosen.length === 1 ? chosen[0] : 'all'}`, text, { paths: chosen, index, target }, widen)
  }

  // A pattern choice also covers files you haven't looked at yet, so the result names the pattern.
  async function chooseEveryVersion(path: string, pattern: string, choice: OverrideChoice, index: number, target: 'row' | 'result', earlier?: { path: string; choice: OverrideChoice } | null) {
    const text =
      choice === 'ship'
        ? `We'll include every file matching ${pattern} in the ${copy.name} pack, along with versions you add later.`
        : `We'll leave every file matching ${pattern} out of the ${copy.name} pack, so players who install that pack won't get them.${dual ? ` They stay in the ${copy.other} pack.` : ''}`
    const alreadyGone = !candidates.some((file) => file.path === path)
    await save([pattern], choice, `${choice}:${path}:every`, text, { paths: [path], index, target }, undefined, earlier ?? undefined)
    if (alreadyGone && target === 'result') {
      refocus = null
      await tick()
      document.getElementById(id('choice-result'))?.focus()
    }
  }

  async function save(chosen: string[], choice: OverrideChoice, key: string, text: string, focus: NonNullable<typeof refocus>, widen?: Widen, restore?: { path: string; choice: OverrideChoice }) {
    choosing = key
    try {
      await api.setOverrideChoice(platform, chosen, choice)
      result = { ok: true, text, undo: { kind: 'files', paths: chosen, restore }, widen }
      refocus = focus
      onchange()
    } catch (error) {
      result = { ok: false, text: `Couldn't save the choice for ${chosen.length === 1 ? chosen[0] : 'those files'}: ${errorText(error)}` }
    } finally {
      choosing = null
    }
  }

  const fileName = (path: string) => path.slice(path.lastIndexOf('/') + 1)

  async function undo(undo: Undo) {
    choosing = 'undo'
    try {
      if (undo.kind === 'files') {
        await api.setOverrideChoice(platform, undo.paths, null)
        if (undo.restore) await api.setOverrideChoice(platform, [undo.restore.path], undo.restore.choice)
      } else if (undo.kind === 'build') {
        await api.useModrinthBuild(undo.path, false)
      } else {
        await api.setModrinthAuthor(undo.author, false)
      }
      result = { ok: true, text: undo.kind === 'author' ? `Undone. We'll stop including everything by ${undo.author} in the Modrinth pack.` : 'Undone. Decide again below.' }
      onchange()
    } catch (error) {
      result = { ok: false, text: `Couldn't undo that: ${errorText(error)}` }
    } finally {
      choosing = null
    }
  }

  async function shipByAuthor(author: string, covered: string[]) {
    choosing = `author:${author}`
    try {
      await api.setModrinthAuthor(author, true)
      result = { ok: true, text: `We'll include everything by ${author} in the Modrinth pack, along with mods of theirs you add later.`, undo: { kind: 'author', author } }
      permittingAuthor = ''
      refocus = { paths: covered, index: 0, target: 'result' }
      onchange()
    } catch (error) {
      result = { ok: false, text: `Couldn't save ${author}'s permission: ${errorText(error)}` }
    } finally {
      choosing = null
    }
  }

  // The swap goes through the same queue as an update in Mods, and the next mod list refresh finds
  // Modrinth's copy of the new file.
  async function switchVersion(file: OverrideCandidate, fileId: number, version: string) {
    const name = file.name ?? file.path
    const serial = startChange([file.path], [name], 'update')
    result = { ok: true, text: `Switching ${name} to ${version}. We'll drop its row once Modrinth's copy matches.` }
    try {
      changeQueued(serial, await api.changeMods([{ file: `cf:${fileId}`, replaces: file.path }]))
    } catch (error) {
      dropChange(serial)
      result = { ok: false, text: `Couldn't switch ${name}: ${errorText(error)}` }
    }
  }

  async function useModrinthBuild(file: OverrideCandidate, version: string, index: number) {
    const name = file.name ?? file.path
    choosing = `build:${file.path}`
    try {
      await api.useModrinthBuild(file.path, true)
      result = { ok: true, text: `We'll use Modrinth's build of ${name} ${version} in the Modrinth pack. CurseForge's build stays in your mods folder and the CurseForge pack.`, undo: { kind: 'build', path: file.path } }
      refocus = { paths: [file.path], index, target: 'row' }
      onchange()
    } catch (error) {
      result = { ok: false, text: `Couldn't use Modrinth's build of ${name}: ${errorText(error)}` }
    } finally {
      choosing = null
    }
  }

  $effect(() => {
    const current = candidates.map((file) => file.path)
    if (!refocus || current.some((path) => refocus!.paths.includes(path))) return
    const index = Math.min(refocus.index, current.length - 1)
    const target = refocus.target === 'result' || index < 0 ? id('choice-result') : refocus.target === 'row' ? id(`row-${index}`) : id(`leaveOut-${index}`)
    refocus = null
    settling = true
    setTimeout(() => (settling = false), SETTLE_MS)
    tick().then(() => document.getElementById(target)?.focus())
  })

  // What Modrinth has instead of this file, and what you can do about it. Moving both packs back a
  // version is never the primary action, since it changes the CurseForge pack too.
  function describeSkew(skew: Skew): { text: string; actions: SkewAction[] } {
    const { shared, modrinthNewest, modrinthNewer, newestOnCurseforge, modrinthBuild: build } = skew
    const sentences: string[] = []
    const actions: SkewAction[] = []
    if (build) {
      sentences.push(`Modrinth has its own build of ${build.version}, which differs from CurseForge's in ${differenceSummary(build.differences)}.`)
      actions.push({
        kind: 'build',
        version: build.version,
        label: `Use Modrinth's ${build.version}`,
        primary: true,
        tip: `List Modrinth's build in the Modrinth pack, so players download it from Modrinth. CurseForge's build stays in your mods folder and the CurseForge pack, so check that the differences don't matter first.`,
      })
    } else if (shared) {
      sentences.push("Modrinth doesn't have this version.")
    }
    if (shared) {
      const match = shared.identical ? 'as identical files' : "with the same contents, though the files' hashes differ"
      sentences.push(shared.newer ? `Both platforms have ${shared.version}, a newer version, ${match}.` : `Both platforms have an older version, ${shared.version}, ${match}.`)
      actions.push({
        kind: 'switch',
        fileId: shared.fileId,
        version: shared.version,
        label: shared.newer ? `Update both packs to ${shared.version}` : `Move both packs back to ${shared.version}`,
        primary: shared.newer && !build,
        tip: `Replace this jar with ${shared.version} in your mods folder, so players of either pack get ${shared.version}.`,
      })
      if (modrinthNewer && modrinthNewest && !newestOnCurseforge) sentences.push(`Modrinth also has ${modrinthNewest}, which CurseForge doesn't.`)
    }
    if (!build && !shared) {
      sentences.push(
        modrinthNewest
          ? `Modrinth only has ${modrinthNewer ? 'newer' : 'older'} versions, up to ${modrinthNewest}, and CurseForge has none of them.`
          : "Modrinth has no version of it for this pack's Minecraft and loader.",
      )
    }
    return { text: sentences.join(' '), actions }
  }

  // Changed code and a changed texture call for different levels of care, so we count them apart.
  function differenceSummary(paths: string[]) {
    const classes = paths.filter((path) => path.endsWith('.class')).length
    const images = paths.filter((path) => /\.(png|jpe?g|gif|webp)$/i.test(path)).length
    const rest = paths.length - classes - images
    const parts = [
      classes && `${classes} ${classes === 1 ? 'class' : 'classes'}`,
      images && `${images} ${images === 1 ? 'image' : 'images'}`,
      rest && `${rest} ${rest === paths.length ? '' : 'other '}${rest === 1 ? 'file' : 'files'}`,
    ].filter((part): part is string => !!part)
    return parts.length > 1 ? `${parts.slice(0, -1).join(', ')} and ${parts.at(-1)}` : parts[0]
  }
</script>

{#if candidates.length}
  <Notice tone="warn" live={false}>
    <div class="mod-list-notice">
      <p>
        <strong>{notOnHostsSummary(paths, copy.name)}.</strong>
        Include the ones {platform === 'modrinth' ? 'you may share' : 'CurseForge allows'}, and leave out the rest. Players who install the {copy.name} pack won't get the
        ones you leave out{#if dual}, though those stay in the {copy.other} pack{/if}.
      </p>
      <details class="how">
        <summary>{copy.how}</summary>
        <p>
          {#if platform === 'modrinth'}
            Modrinth's moderators check that you may share each file you include inside a pack, through its license, its project page or its author, and they
            may ask you for proof. Including a file without an open license rests on your word.
          {:else}
            CurseForge automatically rejects a pack with a mod jar inside that CurseForge doesn't host, unless the mod is on its list of allowed mods or
            CurseForge gave you permission to include it.
          {/if}
          <button class="link" onclick={() => openUrl(copy.guide)}>{copy.guideLabel}</button>
        </p>
      </details>
      {#if vouched.length > 1 || authors.length || candidates.length > 1}
        <div class="reasons">
          {#if vouched.length > 1}
            {@const names = vouched.map((file) => file.name ?? file.path)}
            <button
              class:busy={choosing === 'ship:all'}
              disabled={busy}
              onclick={() => choose(vouched.map((file) => file.path), 'ship', 0, names.length <= 3 ? names.join(', ') : `the ${names.length} files ${platform === 'modrinth' ? 'with open licenses' : "on CurseForge's list"}`, 'result')}
            >
              Include the {vouched.length} {platform === 'modrinth' ? 'with open licenses' : "on CurseForge's list"}
            </button>
          {/if}
          {#if authors.length}
            {@const covered = permittingAuthor ? filesBy(permittingAuthor) : []}
            <span class="by-author">
              <label for={id('permitting-author')}>An author's permission</label>
              <select id={id('permitting-author')} bind:value={permittingAuthor}>
                <option value="" disabled>Choose an author</option>
                {#each authors as author (author)}
                  {@const count = filesBy(author).length}
                  <option value={author}>{author} ({count} {count === 1 ? 'file' : 'files'})</option>
                {/each}
              </select>
              <button class:busy={choosing === `author:${permittingAuthor}`} disabled={busy || !permittingAuthor} onclick={() => shipByAuthor(permittingAuthor, covered)}>
                {permittingAuthor ? `Include ${permittingAuthor}'s ${covered.length === 1 ? 'file' : `${covered.length} files`} and future ones` : 'Include their files and future ones'}
              </button>
            </span>
          {/if}
          {#if candidates.length > 1}
            <button class:busy={choosing === 'leaveOut:all'} disabled={busy} onclick={() => choose(paths, 'leaveOut', 0, `${paths.length} files`, 'result')}>Leave out all {paths.length}</button>
          {/if}
        </div>
      {/if}
      <ul class="decisions" class:all={showAll} aria-label="Files not on {copy.name}">
        {#each shown as file, index (file.path)}
          {@const label = file.name ?? file.path}
          {@const skew = file.skew ? describeSkew(file.skew) : null}
          {@const switching = changeFor(file.path)}
          {@const resolves = !!skew?.actions.some((action) => action.primary)}
          <li id={id(`row-${index}`)} tabindex="-1">
            <div class="what">
              {#if file.name && file.url}
                <a href={file.url} onclick={(e) => { e.preventDefault(); openUrl(file.url!) }}>{file.name}<span class="visually-hidden"> on {copy.other}</span></a>
              {:else if file.name}
                <span>{file.name}</span>
              {/if}
              <span class="facts muted">
                {#if file.name}
                  {#if file.authors.length}by {file.authors.join(', ')} ·{/if}
                  {#if file.license}{file.license}{:else if file.known || !checking}No license listed{:else}Looking up its license{/if}
                  {#if platform === 'modrinth' && file.openLicense}{' '}<span class="chip ok">open license</span>{/if}
                  {#if platform === 'curseforge' && file.allowlisted}{' '}<span class="chip ok">on CurseForge's list</span>{/if}
                {:else}
                  packraven doesn't know which project this file is from.
                {/if}
              </span>
              {#if file.skew && skew}
                {@const differences = file.skew.modrinthBuild?.differences ?? []}
                <div class="skew">
                  {skew.text}{#if file.locked && skew.actions.some((action) => action.kind === 'switch')}{' '}You locked its version in Mods.{/if}
                  <span class="muted">
                    On Modrinth as <a href={file.skew.link.url} onclick={(e) => { e.preventDefault(); openUrl(file.skew!.link.url) }}>{file.skew.link.title}</a>. We linked the two projects
                    because {EVIDENCE[file.skew.link.evidence]}.
                  </span>
                  {#if differences.length}
                    <details class="differences">
                      <summary>What differs</summary>
                      <ul aria-label="Files that differ in Modrinth's build of {label}">
                        {#each differences.slice(0, DIFFERENCES_SHOWN) as path (path)}
                          <li class="file">{path}</li>
                        {/each}
                      </ul>
                      {#if differences.length > DIFFERENCES_SHOWN}
                        <p class="muted">and {differences.length - DIFFERENCES_SHOWN} more</p>
                      {/if}
                    </details>
                  {/if}
                </div>
              {:else if platform === 'modrinth' && !file.versionsKnown && checking}
                <div class="skew muted">Checking which versions Modrinth has</div>
              {/if}
              {#if file.earlier && file.anyVersion}
                <span class="earlier muted">You {file.earlier.choice === 'ship' ? 'included' : 'left out'} another version before: <span class="file">{fileName(file.earlier.path)}</span></span>
              {/if}
              <span class="file" class:muted={!!file.name}>{file.path}</span>
            </div>
            <span class="choices">
              {#each skew?.actions ?? [] as action (action.kind)}
                <Tip tip={action.tip}>
                  {#snippet children(why)}
                    {#if action.kind === 'build'}
                      <button class:primary={action.primary} class:busy={choosing === `build:${file.path}`} disabled={busy} aria-describedby={why} onclick={() => useModrinthBuild(file, action.version, index)}>
                        {action.label}<span class="visually-hidden"> of {label} in the Modrinth pack</span>
                      </button>
                    {:else}
                      <button
                        class:primary={action.primary}
                        class:busy={!!switching && switching.stage !== 'finished'}
                        disabled={busy || !!switching}
                        aria-describedby={why}
                        onclick={() => switchVersion(file, action.fileId!, action.version)}
                      >
                        {switching ? 'Switching' : action.label}<span class="visually-hidden">: {label}</span>
                      </button>
                    {/if}
                  {/snippet}
                </Tip>
              {/each}
              {#if file.anyVersion && (file.earlier || !resolves)}
                {@const choice = file.earlier?.choice ?? 'ship'}
                {@const pattern = file.anyVersion}
                {@const everyTip = file.earlier
                  ? `Save one choice for every file matching ${pattern}, so you won't have to decide again when you update it. This replaces your choice about ${fileName(file.earlier.path)}.`
                  : `${includeTip(file.path)} Saves ${pattern}, so you won't have to decide again when you update it.`}
                <Tip tip={everyTip}>
                  {#snippet children(why)}
                    <button
                      class:primary={!resolves && choice === 'ship' && !!file.earlier}
                      class:busy={choosing === `${choice}:${file.path}:every`}
                      disabled={busy}
                      aria-describedby={why}
                      onclick={() => chooseEveryVersion(file.path, pattern, choice, index, 'row', file.earlier)}
                    >
                      {choice === 'ship' ? 'Include' : 'Leave out'} every version<span class="visually-hidden"> of {label}</span>
                    </button>
                  {/snippet}
                </Tip>
              {/if}
              <Tip tip={includeTip(file.path)}>
                {#snippet children(why)}
                  <button id={id(`ship-${index}`)} class:busy={choosing === `ship:${file.path}`} disabled={busy} aria-describedby={why} onclick={() => choose([file.path], 'ship', index, label, 'row')}>
                    Include {fileKind(file.path)}<span class="visually-hidden">: {label} in the {copy.name} pack</span>
                  </button>
                {/snippet}
              </Tip>
              <Tip tip={leaveOutTip}>
                {#snippet children(why)}
                  <button id={id(`leaveOut-${index}`)} class:busy={choosing === `leaveOut:${file.path}`} disabled={busy} aria-describedby={why} onclick={() => choose([file.path], 'leaveOut', index, label, 'leaveOut')}>
                    Leave out<span class="visually-hidden"> {label} from the {copy.name} pack</span>
                  </button>
                {/snippet}
              </Tip>
            </span>
          </li>
        {/each}
      </ul>
      {#if candidates.length > shown.length}
        <button class="link show-all" onclick={showMore}>Show {candidates.length - shown.length} more</button>
      {/if}
    </div>
  </Notice>
{/if}
<!-- Mounted all along, so screen readers announce each result as it changes. Hidden while empty, so it
     doesn't add a gap between the notices. -->
<p id={id('choice-result')} tabindex="-1" class:visually-hidden={!result} class:muted={result?.ok} class:failed={result && !result.ok} role="status">
  {#if result}
    {result.text}
    {#if result.undo}
      {@const last = result.undo}
      {@const widen = result.widen}
      {' '}<button class="link" disabled={!!choosing} onclick={() => undo(last)}>Undo</button>{#if last.kind === 'build'}.{:else},
        {#if widen}
          {@const action = widen.choice === 'ship' ? 'include' : 'leave out'}
          <button class="link" disabled={!!choosing} onclick={() => chooseEveryVersion(widen.path, widen.pattern, widen.choice, 0, 'result', widen.earlier)}>{action} every version matching <span class="file">{widen.pattern}</span></button>,
        {/if}
        or change it later in
        <button class="link" onclick={onsettings}>Pack settings</button>.
      {/if}
    {/if}
  {/if}
</p>

<style>
  .mod-list-notice {
    display: grid;
    gap: 10px;
  }

  .mod-list-notice > p {
    max-width: 56ch;
  }

  /* Notice caps the height of its lists for plain file names, but we show at most a few of these
     taller rows until you ask for all. */
  .mod-list-notice .decisions {
    display: grid;
    gap: 8px;
    max-height: none;
    overflow: visible;
    margin: 6px 0 0;
    padding: 0;
    list-style: none;
  }

  .mod-list-notice .decisions.all {
    max-height: 50vh;
    overflow: auto;
  }

  .decisions > li {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 4px 12px;
  }

  /* In a wide window the buttons sit far from a short row's text, so a rule carries the eye across. */
  .decisions > li + li {
    padding-top: 8px;
    border-top: 1px solid color-mix(in srgb, var(--warn-border) 40%, transparent);
  }

  .decisions > li:focus-visible {
    outline-offset: 2px;
  }

  .what {
    display: grid;
    gap: 1px;
    min-width: 0;
    flex: 1 1 320px;
  }

  .what .file,
  .facts,
  .skew,
  .earlier {
    overflow-wrap: anywhere;
    font-size: var(--step--1);
  }

  .skew {
    max-width: 56ch;
    margin: 2px 0;
  }

  .differences summary {
    width: fit-content;
    margin-top: 2px;
    cursor: pointer;
  }

  .differences ul {
    margin: 4px 0 0;
    padding-left: 1.2em;
  }

  .how summary {
    width: fit-content;
    color: var(--text);
    font-size: var(--step--1);
    cursor: pointer;
  }

  .how p {
    max-width: 65ch;
    margin-top: 4px;
    font-size: var(--step--1);
  }

  .reasons {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 14px;
  }

  .by-author {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  .choices {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .show-all {
    justify-self: start;
  }
</style>
