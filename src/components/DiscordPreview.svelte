<script lang="ts">
  import { convertFileSrc } from '@tauri-apps/api/core'
  import { openUrl } from '@tauri-apps/plugin-opener'
  import { api, type ComposedAnnouncement } from '../lib/api'
  import { formatMessageTime, isSafeUrl } from '../lib/discordMarkdown'
  import DiscordMarkdown from './DiscordMarkdown.svelte'

  interface EmbedField {
    name: string
    value: string
    inline?: boolean
  }

  interface Embed {
    title?: string
    url?: string
    description?: string
    color?: number
    author?: { name: string; url?: string | null; icon_url?: string | null }
    footer?: { text: string; icon_url?: string | null }
    timestamp?: string
    image?: { url: string }
    thumbnail?: { url: string }
    fields?: EmbedField[]
  }

  interface Payload {
    username?: string
    avatar_url?: string
    content?: string
    embeds?: Embed[]
  }

  let { composed, label = 'Discord post preview' }: { composed: ComposedAnnouncement; label?: string } = $props()

  let payload = $derived(composed.payload as Payload)
  let rawEmbeds = $derived(payload.embeds ?? [])
  // Discord merges up to 4 embeds sharing the same url into one embed gallery.
  let embeds = $derived.by(() => {
    const merged: Array<{ embed: Embed; images: NonNullable<Embed['image']>[] }> = []
    for (const embed of rawEmbeds) {
      const prev = merged.at(-1)
      if (prev && embed.url && prev.embed.url === embed.url && !embed.title && !embed.description && !embed.fields?.length && embed.image && prev.images.length < 4) {
        prev.images.push(embed.image)
      } else {
        merged.push({ embed, images: embed.image ? [embed.image] : [] })
      }
    }
    return merged
  })
  let username = $derived(payload.username?.trim() || 'Webhook')
  let referenced = $derived(new Set(rawEmbeds.flatMap((embed) => [embed.image?.url, embed.thumbnail?.url, embed.author?.icon_url, embed.footer?.icon_url]).filter((url): url is string => !!url?.startsWith('attachment://')).map((url) => url.slice('attachment://'.length))))
  // Discord shows the attachments no embed uses as files of their own, between the content and the embeds.
  let looseAttachments = $derived(composed.attachments.filter((attachment) => !referenced.has(attachment.filename)))
  const now = new Date()

  // The webview can only load a local file once the backend allows its path, which it does even for
  // an image it then rejects, so we show the image either way.
  let viewable = $state<string[]>([])
  $effect(() => {
    const paths = composed.attachments.map((attachment) => attachment.path)
    if (!paths.length) return
    let stale = false
    const show = () => {
      if (!stale) viewable = paths
    }
    api.checkAnnouncementImages(paths).then(show, show)
    return () => (stale = true)
  })

  function imageSource(url: string | null | undefined): string | null {
    if (!url) return null
    if (url.startsWith('attachment://')) {
      const attachment = composed.attachments.find((a) => a.filename === url.slice('attachment://'.length))
      return attachment && viewable.includes(attachment.path) ? convertFileSrc(attachment.path) : null
    }
    return isSafeUrl(url) || url.startsWith('data:image/') ? url : null
  }

  function attachmentName(url: string) {
    return url.startsWith('attachment://') ? url.slice('attachment://'.length) : ''
  }

  function colorOf(embed: Embed) {
    return typeof embed.color === 'number' ? `#${embed.color.toString(16).padStart(6, '0')}` : null
  }

  function open(event: MouseEvent, url: string) {
    event.preventDefault()
    openUrl(url)
  }

  // Discord puts up to three inline fields side by side and gives every other field a row of its own.
  function fieldColumns(fields: EmbedField[]) {
    const spans: string[] = []
    let index = 0
    while (index < fields.length) {
      if (!fields[index].inline) {
        spans.push('1 / 13')
        index++
        continue
      }
      let run = 0
      while (run < 3 && index + run < fields.length && fields[index + run].inline) run++
      const width = 12 / run
      for (let i = 0; i < run; i++) spans.push(`${1 + i * width} / ${1 + (i + 1) * width}`)
      index += run
    }
    return spans
  }

  function time(timestamp: string) {
    const date = new Date(timestamp)
    return Number.isNaN(date.getTime()) ? null : { iso: date.toISOString(), text: formatMessageTime(date, { now }) }
  }
</script>

<div class="discord" role="region" aria-label={label}>
  <div class="message">
    {#if imageSource(payload.avatar_url)}
      <img class="avatar" src={imageSource(payload.avatar_url)} alt="" />
    {:else}
      <span class="avatar initial" aria-hidden="true">{username.slice(0, 1).toUpperCase()}</span>
    {/if}
    <div class="body">
      <p class="header">
        <span class="username">{username}</span>
        <span class="app" title="Posted by a webhook">APP</span>
        <time class="sent" datetime={now.toISOString()}>{formatMessageTime(now, { now })}</time>
      </p>
      {#if payload.content?.trim()}
        <div class="content"><DiscordMarkdown text={payload.content} jumbo /></div>
      {/if}
      {#if looseAttachments.length}
        <ul class="attachments" class:single={looseAttachments.length === 1} style:--columns={looseAttachments.length === 2 || looseAttachments.length === 4 ? 2 : Math.min(3, looseAttachments.length)} aria-label="Attached images">
          {#each looseAttachments as attachment (attachment.filename)}
            <li>
              {#if viewable.includes(attachment.path)}
                <img src={convertFileSrc(attachment.path)} alt={attachment.filename} />
              {:else}
                <span class="missing">{attachment.filename}</span>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      {#each embeds as { embed, images: embedImages }, index (index)}
        {@const thumbnail = imageSource(embed.thumbnail?.url)}
        {@const authorIcon = imageSource(embed.author?.icon_url)}
        {@const footerIcon = imageSource(embed.footer?.icon_url)}
        {@const sent = embed.timestamp ? time(embed.timestamp) : null}
        {@const spans = fieldColumns(embed.fields ?? [])}
        <article class="embed" style:border-left-color={colorOf(embed)} aria-label={embed.title ? `Embed: ${embed.title}` : 'Embed'}>
          {#if embed.author?.name}
            <p class="author">
              {#if authorIcon}<img src={authorIcon} alt="" />{/if}
              {#if embed.author.url && isSafeUrl(embed.author.url)}
                <a href={embed.author.url} onclick={(e) => open(e, embed.author!.url!)}>{embed.author.name}</a>
              {:else}
                <span>{embed.author.name}</span>
              {/if}
            </p>
          {/if}
          {#if embed.title}
            <p class="title">
              {#if embed.url && isSafeUrl(embed.url)}
                <a href={embed.url} onclick={(e) => open(e, embed.url!)}><DiscordMarkdown text={embed.title} inline /></a>
              {:else}
                <DiscordMarkdown text={embed.title} inline />
              {/if}
            </p>
          {/if}
          {#if embed.description}
            <div class="description"><DiscordMarkdown text={embed.description} /></div>
          {/if}
          {#if embed.fields?.length}
            <dl class="fields">
              {#each embed.fields as field, fieldIndex (fieldIndex)}
                <div class="field" style:grid-column={spans[fieldIndex]}>
                  <dt><DiscordMarkdown text={field.name} inline /></dt>
                  <dd><DiscordMarkdown text={field.value} /></dd>
                </div>
              {/each}
            </dl>
          {/if}
          {#if embedImages.length === 1}
            {@const image = imageSource(embedImages[0].url)}
            {#if image}
              <img class="image" src={image} alt={attachmentName(embedImages[0].url) || 'Embed image'} />
            {:else}
              <span class="image missing">{attachmentName(embedImages[0].url) || 'Image'}</span>
            {/if}
          {:else if embedImages.length > 1}
            <div class="embed-gallery" class:three={embedImages.length === 3}>
              {#each embedImages as item, imageIndex (imageIndex)}
                {@const image = imageSource(item.url)}
                {#if image}
                  <img src={image} alt={attachmentName(item.url) || 'Embed image'} />
                {:else}
                  <span class="missing">{attachmentName(item.url) || 'Image'}</span>
                {/if}
              {/each}
            </div>
          {/if}
          {#if thumbnail}
            <img class="thumbnail" src={thumbnail} alt="Thumbnail" />
          {/if}
          {#if embed.footer?.text || sent}
            <p class="footer">
              {#if footerIcon}<img src={footerIcon} alt="" />{/if}
              <span>{embed.footer?.text ?? ''}{#if embed.footer?.text && sent}<span aria-hidden="true">{' • '}</span>{/if}{#if sent}<time datetime={sent.iso}>{sent.text}</time>{/if}</span>
            </p>
          {/if}
        </article>
      {/each}
    </div>
  </div>
</div>

<style>
  /* We show Discord's dark theme in both of our themes, so the preview matches what most players see. */
  .discord {
    --discord-bg: #313338;
    --discord-embed: #2b2d31;
    --discord-text: #dbdee1;
    --discord-strong: #f2f3f5;
    --discord-muted: #949ba4;
    --discord-link: #00a8fc;
    --discord-code: #2b2d31;
    --discord-code-edge: #1e1f22;
    --discord-quote: #4e5058;
    --discord-mention: rgb(88 101 242 / 0.3);
    --discord-mention-text: #c9cdfb;
    --discord-spoiler: #1e1f22;
    --discord-spoiler-shown: rgb(255 255 255 / 0.1);

    color-scheme: dark;
    max-width: 640px;
    padding: 14px 16px 16px 0;
    font-size: var(--step-0);
    line-height: 1.375;
    color: var(--discord-text);
    background: var(--discord-bg);
    border: 1px solid var(--line-strong);
    border-radius: var(--radius-panel);
  }

  .message {
    display: grid;
    grid-template-columns: 72px minmax(0, 1fr);
  }

  .avatar {
    justify-self: center;
    width: 40px;
    height: 40px;
    border-radius: 50%;
    object-fit: cover;
  }

  .initial {
    display: grid;
    place-items: center;
    font-weight: 700;
    color: #fff;
    background: #5865f2;
  }

  .body {
    min-width: 0;
  }

  p {
    margin: 0;
  }

  .header {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0 6px;
  }

  .username {
    font-weight: 600;
    color: var(--discord-strong);
  }

  .app {
    padding: 0 4px;
    font-size: 10px;
    font-weight: 600;
    line-height: 15px;
    color: #fff;
    background: #5865f2;
    border-radius: 3px;
    align-self: center;
  }

  .sent {
    font-size: 12px;
    color: var(--discord-muted);
  }

  .content {
    margin-top: 2px;
  }

  .attachments {
    display: grid;
    grid-template-columns: repeat(var(--columns), minmax(0, 1fr));
    gap: 4px;
    max-width: 550px;
    margin: 6px 0 0;
    padding: 0;
    list-style: none;
  }

  .attachments img {
    display: block;
    width: 100%;
    height: 160px;
    object-fit: cover;
    border-radius: 4px;
  }

  .attachments.single img {
    width: auto;
    max-width: 100%;
    height: auto;
    max-height: 350px;
    object-fit: contain;
  }

  .missing {
    display: grid;
    place-items: center;
    min-height: 80px;
    padding: 8px;
    font-size: 13px;
    color: var(--discord-muted);
    background: var(--discord-embed);
    border-radius: 4px;
    overflow-wrap: anywhere;
  }

  .embed {
    --discord-code: #1e1f22;

    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: start;
    max-width: 520px;
    margin-top: 6px;
    padding: 8px 16px 16px 12px;
    font-size: var(--step--1);
    background: var(--discord-embed);
    border-left: 4px solid #1e1f22;
    border-radius: 4px;
  }

  .embed > :not(.thumbnail) {
    grid-column: 1;
    min-width: 0;
  }

  .embed > * + :not(.thumbnail) {
    margin-top: 8px;
  }

  .author {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 600;
    color: var(--discord-strong);
  }

  .author img {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    object-fit: cover;
  }

  .author a {
    color: inherit;
    text-decoration: none;
  }

  .title {
    font-size: var(--step-0);
    font-weight: 600;
    color: var(--discord-strong);
  }

  .title a,
  .author a {
    text-decoration: none;
  }

  .title a {
    color: var(--discord-link);
  }

  .title a:hover,
  .author a:hover {
    text-decoration: underline;
  }

  .fields {
    display: grid;
    grid-template-columns: repeat(12, minmax(0, 1fr));
    gap: 8px;
    margin-bottom: 0;
  }

  .field dt {
    font-weight: 600;
    color: var(--discord-strong);
  }

  .field dd {
    margin: 2px 0 0;
  }

  .embed .image {
    grid-column: 1 / 3;
    max-width: 100%;
    max-height: 300px;
    margin-top: 16px;
    border-radius: 4px;
    object-fit: contain;
    justify-self: start;
  }

  .embed .image.missing {
    width: 100%;
  }

  .embed-gallery {
    grid-column: 1 / 3;
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 4px;
    margin-top: 16px;
    border-radius: 4px;
    overflow: hidden;
  }

  .embed-gallery img,
  .embed-gallery .missing {
    width: 100%;
    height: 100%;
    aspect-ratio: 16 / 10;
    object-fit: cover;
    display: block;
  }

  .embed-gallery.three > :first-child {
    grid-row: span 2;
    aspect-ratio: auto;
  }

  .thumbnail {
    grid-column: 2;
    grid-row: 1 / span 6;
    width: 80px;
    height: 80px;
    margin: 8px 0 0 16px;
    border-radius: 4px;
    object-fit: contain;
  }

  .embed > .footer {
    grid-column: 1 / 3;
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--discord-muted);
  }

  .footer img {
    width: 20px;
    height: 20px;
    border-radius: 50%;
  }

  .discord a:focus-visible {
    outline: 2px solid var(--discord-link);
    outline-offset: 1px;
    border-radius: 2px;
  }
</style>
