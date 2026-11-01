<script lang="ts">
  import { ask } from '@tauri-apps/plugin-dialog'
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener'
  import { api, errorText, type ServerPackUpload } from '../lib/api'
  import { job, onJobEnd } from '../lib/jobs.svelte'
  import LinkedText from './LinkedText.svelte'

  let {
    upload,
    version,
    clientName,
    retry = true,
    tagged = true,
    onretry,
    onmarked,
  }: {
    upload: ServerPackUpload
    version: string
    clientName: string
    // Off where another control already uploads it, such as Finish publishing, so this shows only the manual route.
    retry?: boolean
    // Whether the release has its tag yet, which you'd check out to rebuild the server pack as it shipped.
    tagged?: boolean
    onretry?: () => void
    onmarked?: () => void
  } = $props()

  let error = $state<string | null>(null)
  let retrying = $state(false)
  let copied = $state(false)
  let zipName = $derived(upload.zip?.split(/[\\/]/).pop() ?? null)
  // CurseForge's messages sometimes end in their own period.
  let reason = $derived(upload.error?.trim().replace(/[.!]+$/, '') ?? null)
  // packraven rebuilds a zip that's still there only when CurseForge turned it down as too large.
  let tooLarge = $derived(upload.rebuild && !!upload.zip)
  // Any publish job uses the same uploads, so none of these controls should start a second one.
  let busy = $derived(job('publish').phase === 'running')

  // A retry runs as a publish job, so we hear how it went the same way Publish does.
  onJobEnd('publish', (phase) => {
    if (!retrying) return
    retrying = false
    if (phase === 'failed') error = `Couldn't upload it: ${job('publish').message ?? 'the upload failed'}`
  })

  async function retryUpload() {
    if (upload.rebuild && upload.packChanged) {
      const confirmed = await ask(`Your mods changed since ${version} shipped, so a server pack built now won't match the client pack on CurseForge.`, {
        title: `Build the ${version} server pack again`,
        kind: 'warning',
        okLabel: 'Build and upload anyway',
        cancelLabel: 'Cancel',
      })
      if (!confirmed) return
    }
    error = null
    retrying = true
    onretry?.()
    try {
      await api.uploadReleaseServerPack(version)
    } catch (e) {
      retrying = false
      error = `Couldn't upload it: ${errorText(e)}`
    }
  }

  async function markUploaded() {
    try {
      await api.markServerPackUploaded(version)
      onmarked?.()
    } catch (e) {
      error = `Couldn't record it: ${errorText(e)}`
    }
  }

  function showZip() {
    revealItemInDir(upload.zip!).catch((e) => (error = `Couldn't open the folder: ${errorText(e)}`))
  }

  function openUploadPage() {
    openUrl(upload.uploadPage).catch((e) => (error = `Couldn't open CurseForge: ${errorText(e)}`))
  }

  async function copyName() {
    try {
      await navigator.clipboard.writeText(upload.displayName!)
      copied = true
      setTimeout(() => (copied = false), 2000)
    } catch (e) {
      error = `Couldn't copy the name: ${errorText(e)}`
    }
  }
</script>

{#if retry}
  <p class="muted">
    {#if reason}<LinkedText text={`${reason}.`} />{/if}
    Server owners can't download {version} from CurseForge until its server pack is there.
  </p>
  {#if upload.retryable}
    <p class="retry">
      <button class:busy={retrying} disabled={busy} onclick={retryUpload}>{retrying ? 'Uploading' : upload.rebuild ? 'Build it again and upload it' : 'Upload it again'}</button>
      <span class="muted small">or upload it yourself:</span>
    </p>
  {/if}
  {#if upload.rebuild}
    <p class="muted small measure">
      {upload.zip ? 'That zip is too large to upload' : `packraven no longer has the server pack it built for ${version}`}, so we build it again from the pack as it is now, with its
      mods downloaded on first start.
      {#if upload.packChanged && tagged}Your mods changed since {version} shipped, so check out <span class="file">{upload.tag}</span> in Git first if the server pack has to match.{/if}
    </p>
  {/if}
{/if}
<ol class="steps">
  {#if upload.zip}
    <li>
      <div class="step">
        <button disabled={busy} onclick={showZip}>Show server pack</button>
        <span class="file">{zipName}</span>
        {#if upload.fileName && upload.fileName !== zipName}
          <span class="muted small">packraven would have uploaded it as <span class="file">{upload.fileName}</span>.</span>
        {/if}
      </div>
    </li>
  {/if}
  <li>
    <div class="step">
      <button disabled={busy} onclick={openUploadPage}>Open CurseForge's upload page</button>
      <span class="muted small">
        Upload the zip there as an additional file of {clientName}{#if upload.displayName}, named "{upload.displayName}".
          <button type="button" class="link" onclick={copyName}>{copied ? 'Copied' : 'Copy name'}</button>{:else}.{/if}
      </span>
      {#if tooLarge}<span class="muted small">CurseForge appears to cap uploads at 1 GB, so its website will likely turn this file down too.</span>{/if}
    </div>
  </li>
  <li>
    <div class="step">
      <button disabled={busy} onclick={markUploaded}>I uploaded it</button>
      <span class="muted small measure">
        We stop offering to upload it. CurseForge only links the server pack once the client pack is public, and Releases shows it then.
      </span>
    </div>
  </li>
</ol>
<span class="visually-hidden" role="status">{copied ? `Copied "${upload.displayName}"` : ''}</span>
{#if error}<p class="failed small" role="alert"><LinkedText text={error} /></p>{/if}

<style>
  .retry {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 10px;
  }

  .steps {
    display: grid;
    gap: 10px;
    margin: 2px 0;
    padding-left: 20px;
  }

  .steps li {
    padding-left: 4px;
  }

  .step {
    display: grid;
    justify-items: start;
    gap: 4px;
  }

  .steps li::marker {
    color: var(--muted);
    font-size: var(--step--1);
  }

  .file {
    overflow-wrap: anywhere;
  }

  .link {
    margin-left: 4px;
  }

  .small {
    font-size: var(--step--1);
  }

  /* ch is the width of a 0, wider than this font's average letter, so 52ch keeps small lines near 80 characters. */
  .measure {
    max-width: 52ch;
  }
</style>
