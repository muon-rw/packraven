<script lang="ts">
  import { open } from '@tauri-apps/plugin-dialog'
  import ServerListEntry from './ServerListEntry.svelte'
  import { api, errorText } from '../lib/api'
  import { defaultMotd, fillMotd, motdColors, motdFromProperty, motdPlaceholders, motdStyles, motdToProperty } from '../lib/motd'

  let {
    properties,
    packName,
    version,
    minecraft,
    icon = $bindable(),
    hasProject,
    testPort,
    onchange,
    oniconchange,
  }: {
    properties: Record<string, string>
    packName: string
    version: string | null
    minecraft: string | null
    icon: string | null
    hasProject: boolean
    testPort: number
    onchange: (properties: Record<string, string>) => void
    oniconchange: () => void
  } = $props()

  type Field = {
    key: string
    label: string
    vanilla: string
    kind: 'bool' | 'number' | 'text' | 'choice'
    choices?: [string, string][]
    min?: number
    max?: number
    unit?: string
    placeholder?: string
    hint?: string
  }

  // Vanilla's defaults, so we only write what you change and leave the rest to the server.
  let groups: { title: string; fields: Field[] }[] = $derived([
    {
      title: 'Gameplay',
      fields: [
        { key: 'gamemode', label: 'Game mode', kind: 'choice', vanilla: 'survival', choices: [['survival', 'Survival'], ['creative', 'Creative'], ['adventure', 'Adventure'], ['spectator', 'Spectator']] },
        { key: 'difficulty', label: 'Difficulty', kind: 'choice', vanilla: 'easy', choices: [['peaceful', 'Peaceful'], ['easy', 'Easy'], ['normal', 'Normal'], ['hard', 'Hard']] },
        { key: 'hardcore', label: 'Hardcore', kind: 'bool', vanilla: 'false' },
        { key: 'pvp', label: 'Players can hurt each other', kind: 'bool', vanilla: 'true' },
        { key: 'allow-flight', label: 'Allow flight', kind: 'bool', vanilla: 'false', hint: 'Without it, vanilla kicks players who fly with jetpacks, wings and similar mods.' },
        { key: 'allow-nether', label: 'Allow the Nether', kind: 'bool', vanilla: 'true' },
        { key: 'enable-command-block', label: 'Enable command blocks', kind: 'bool', vanilla: 'false' },
      ],
    },
    {
      title: 'World',
      fields: [
        { key: 'level-name', label: 'World folder', kind: 'text', vanilla: 'world' },
        { key: 'level-seed', label: 'Seed', kind: 'text', vanilla: '', placeholder: 'Random' },
        { key: 'level-type', label: 'World type', kind: 'text', vanilla: 'minecraft:normal' },
        { key: 'view-distance', label: 'View distance', kind: 'number', vanilla: '10', min: 3, max: 32, unit: 'chunks' },
        { key: 'simulation-distance', label: 'Simulation distance', kind: 'number', vanilla: '10', min: 3, max: 32, unit: 'chunks' },
        { key: 'spawn-protection', label: 'Spawn protection', kind: 'number', vanilla: '16', min: 0, unit: 'blocks' },
      ],
    },
    {
      title: 'Access',
      fields: [
        { key: 'white-list', label: 'Only let whitelisted players join', kind: 'bool', vanilla: 'false' },
        { key: 'enforce-whitelist', label: 'Kick players you remove from the whitelist', kind: 'bool', vanilla: 'false' },
        { key: 'online-mode', label: 'Online mode', kind: 'bool', vanilla: 'true', hint: "Vanilla checks each player's Minecraft account. Test servers use this value too." },
        { key: 'server-port', label: 'Port', kind: 'number', vanilla: '25565', min: 1, max: 65535, hint: `Test runs use port ${testPort} from Pack settings instead.` },
      ],
    },
    {
      title: 'Performance',
      fields: [
        { key: 'max-tick-time', label: 'Watchdog timeout', kind: 'number', vanilla: '60000', min: -1, unit: 'ms', hint: 'Vanilla stops a server that spends this long on one tick. Enter -1 to turn the watchdog off.' },
      ],
    },
  ])

  const isChanged = (field: Field) => field.key in properties && properties[field.key] !== field.vanilla

  let known = $derived(new Set(['motd', 'max-players', ...groups.flatMap((g) => g.fields.map((f) => f.key))]))
  let others = $derived(Object.entries(properties).filter(([key]) => !known.has(key)))

  function set(key: string, value: string, vanilla: string | null) {
    const next = { ...properties }
    if (value === vanilla || (vanilla !== null && value.trim() === '')) delete next[key]
    else next[key] = value
    onchange(next)
  }

  function rename(from: string, to: string) {
    const key = to.trim()
    if (!key || key === from) return
    const next: Record<string, string> = {}
    for (const [k, v] of Object.entries(properties)) next[k === from ? key : k] = v
    onchange(next)
  }

  function remove(key: string) {
    const next = { ...properties }
    delete next[key]
    onchange(next)
  }

  let newKey = $state('')
  let newValue = $state('')
  let newKeyField = $state<HTMLInputElement>()

  function add() {
    const key = newKey.trim()
    if (!key) return newKeyField?.focus()
    onchange({ ...properties, [key]: newValue })
    newKey = ''
    newValue = ''
    newKeyField?.focus()
  }

  let motdField = $state<HTMLTextAreaElement>()
  let motd = $derived(motdFromProperty(properties.motd ?? ''))
  let shownMotd = $derived(fillMotd(motd || defaultMotd, { name: packName, version: version ?? '1.0.0', minecraft: minecraft ?? '1.21.1' }))
  let motdLineCount = $derived(motd.split('\n').length)

  function saveMotd(text: string) {
    if (text.trim()) set('motd', motdToProperty(text), null)
    else remove('motd')
  }

  function insert(inserted: string) {
    const field = motdField
    if (!field) return
    const [start, end] = [field.selectionStart, field.selectionEnd]
    saveMotd(field.value.slice(0, start) + inserted + field.value.slice(end))
    requestAnimationFrame(() => {
      field.focus()
      field.setSelectionRange(start + inserted.length, start + inserted.length)
    })
  }

  let iconBusy = $state<'pick' | 'logo' | null>(null)
  let iconError = $state<string | null>(null)

  async function changeIcon(how: 'pick' | 'logo' | 'remove') {
    iconError = null
    try {
      if (how === 'remove') {
        await api.removeServerIcon()
        icon = null
      } else if (how === 'pick') {
        const image = await open({ title: 'Choose a server icon', filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'gif', 'webp'] }] })
        if (typeof image !== 'string') return
        iconBusy = 'pick'
        icon = await api.setServerIcon(image)
      } else {
        iconBusy = 'logo'
        icon = await api.useProjectLogoAsServerIcon()
      }
      oniconchange()
    } catch (error) {
      iconError = `Couldn't change the icon: ${errorText(error)}`
    } finally {
      iconBusy = null
    }
  }
</script>

<!-- We keep an unchanged field's Reset in place but hidden, so its control doesn't resize when you edit it. -->
{#snippet reset(field: Field)}
  {@const changed = isChanged(field)}
  <button
    type="button"
    class="ghost reset"
    class:unchanged={!changed}
    disabled={!changed}
    aria-hidden={!changed}
    tabindex={changed ? 0 : -1}
    onclick={() => remove(field.key)}
    aria-label="Reset {field.label} to vanilla's {field.kind === 'bool' ? (field.vanilla === 'true' ? 'on' : 'off') : field.vanilla || 'empty value'}">Reset</button
  >
{/snippet}

<div class="properties">
  <section class="listing" aria-labelledby="listing-title">
    <div class="preview">
      <h3 id="listing-title">Server list</h3>
      <ServerListEntry {icon} name={packName} motd={shownMotd} maxPlayers={properties['max-players'] ?? '20'} />
      <p class="hint">How the server appears in a player's multiplayer list. Players name the server themselves, so we show the pack name.</p>
    </div>

    <div class="listing-fields">
      <div class="field">
        <span class="label" id="icon-label">Icon</span>
        <div class="buttons" role="group" aria-labelledby="icon-label">
          <button type="button" class:busy={iconBusy === 'pick'} disabled={!!iconBusy} onclick={() => changeIcon('pick')}>Choose image</button>
          <button type="button" class:busy={iconBusy === 'logo'} disabled={!!iconBusy || !hasProject} onclick={() => changeIcon('logo')}>Use CurseForge logo</button>
          {#if icon}<button type="button" class="quiet" disabled={!!iconBusy} onclick={() => changeIcon('remove')}>Remove</button>{/if}
        </div>
        <p class="hint">
          packraven crops the image to a square, scales it to the 64×64 PNG vanilla needs and saves it as <span class="file">server-icon.png</span> in the pack
          repo.{' '}{#if !hasProject}To use the CurseForge logo, set the project ID in Pack settings first.{/if}
        </p>
        {#if iconError}<p class="failed" role="alert">{iconError}</p>{/if}
      </div>

      <div class="field">
        <label for="motd">Message of the day</label>
        <div class="codes" role="toolbar" aria-label="Insert a formatting code" aria-controls="motd">
          <span class="code-group">
          {#each motdColors as [code, name, color] (code)}
            <button type="button" class="swatch" style:--swatch={color} title="{name} (§{code})" aria-label="{name}, §{code}" onclick={() => insert(`§${code}`)}></button>
          {/each}
          </span>
          <span class="code-group">
          {#each motdStyles as [code, name] (code)}
            <button type="button" class="style {name.toLowerCase()}" title="{name} (§{code})" aria-label="{name}, §{code}" onclick={() => insert(`§${code}`)}>
              {name === 'Reset' ? 'Reset' : name === 'Obfuscated' ? '?#' : name[0]}
            </button>
          {/each}
          </span>
          <span class="code-group">
          {#each motdPlaceholders as [placeholder, name] (placeholder)}
            <button type="button" class="placeholder" title="Insert {name}" aria-label="{name}, {placeholder}" onclick={() => insert(placeholder)}>{placeholder}</button>
          {/each}
          </span>
        </div>
        <textarea
          id="motd"
          rows="2"
          bind:this={motdField}
          value={motd}
          placeholder={defaultMotd}
          spellcheck="false"
          aria-describedby="motd-hint"
          onchange={(e) => saveMotd(e.currentTarget.value)}
        ></textarea>
        <p class="hint" id="motd-hint">
          {#if motdLineCount > 2}<span class="warn">Players only see the first two lines.</span>{/if}
          Pick a color or style to insert its § code at the cursor. We fill in {'{name}'}, {'{version}'} and {'{minecraft}'} each time we build a server pack, so
          leaving it empty shows the pack name and the version you're building.
        </p>
      </div>

      <label class="short">
        Most players at once
        <input type="number" min="0" value={properties['max-players'] ?? '20'} onchange={(e) => set('max-players', e.currentTarget.value, '20')} />
      </label>
    </div>
  </section>

  {#each groups as group (group.title)}
    {@const changedCount = group.fields.filter((f) => isChanged(f)).length}
    <section class="group" aria-labelledby="group-{group.title}">
      <div class="group-head">
        <h3 id="group-{group.title}">{group.title}</h3>
        {#if changedCount}<span class="chip change">{changedCount} changed from vanilla</span>{/if}
      </div>
      <div class="grid">
        {#each group.fields.filter((f) => f.kind !== 'bool') as field (field.key)}
          {@const value = properties[field.key] ?? field.vanilla}
          <div class="field">
            <label for="prop-{field.key}">{field.label}</label>
            <div class="control">
              {#if field.kind === 'choice'}
                <select id="prop-{field.key}" {value} onchange={(e) => set(field.key, e.currentTarget.value, field.vanilla)} aria-describedby={field.hint ? `hint-${field.key}` : undefined}>
                  {#each field.choices ?? [] as [id, label] (id)}<option value={id}>{label}{id === field.vanilla ? ' (vanilla)' : ''}</option>{/each}
                  {#if !(field.choices ?? []).some(([id]) => id === value)}<option {value}>{value}</option>{/if}
                </select>
              {:else}
                <input
                  id="prop-{field.key}"
                  type={field.kind === 'number' ? 'number' : 'text'}
                  min={field.min}
                  max={field.max}
                  {value}
                  placeholder={field.placeholder ?? field.vanilla}
                  spellcheck="false"
                  class:mono={field.kind === 'text'}
                  aria-describedby={field.hint ? `hint-${field.key}` : undefined}
                  onchange={(e) => set(field.key, e.currentTarget.value, field.vanilla)}
                />
              {/if}
              {#if field.unit}<span class="unit">{field.unit}</span>{/if}
              {@render reset(field)}
            </div>
            {#if field.hint}<p class="hint" id="hint-{field.key}">{field.hint}</p>{/if}
          </div>
        {/each}
      </div>
      {#if group.fields.some((f) => f.kind === 'bool')}
        <div class="checks">
          {#each group.fields.filter((f) => f.kind === 'bool') as field (field.key)}
            <div class="field">
              <div class="check-row">
                <label class="check">
                  <input type="checkbox" checked={(properties[field.key] ?? field.vanilla) === 'true'} onchange={(e) => set(field.key, String(e.currentTarget.checked), field.vanilla)} aria-describedby={field.hint ? `hint-${field.key}` : undefined} />
                  {field.label}
                </label>
                {@render reset(field)}
              </div>
              {#if field.hint}<p class="hint indent" id="hint-{field.key}">{field.hint}</p>{/if}
            </div>
          {/each}
        </div>
      {/if}
    </section>
  {/each}

  <section class="group" aria-labelledby="others-title">
    <h3 id="others-title">Other properties</h3>
    <p class="hint">Any other <span class="file">server.properties</span> key, written as you enter it. Mods with their own properties read them from here too.</p>
    <div class="others">
      {#each others as [key, value] (key)}
        <div class="pair">
          <input class="mono" value={key} aria-label="Key" spellcheck="false" onchange={(e) => rename(key, e.currentTarget.value)} />
          <span class="equals" aria-hidden="true">=</span>
          <input class="mono" {value} aria-label="Value of {key}" spellcheck="false" onchange={(e) => set(key, e.currentTarget.value, null)} />
          <button type="button" class="quiet" onclick={() => remove(key)} aria-label="Remove {key}">Remove</button>
        </div>
      {/each}
      <form class="pair" onsubmit={(e) => { e.preventDefault(); add() }}>
        <input class="mono" bind:this={newKeyField} bind:value={newKey} placeholder="key" aria-label="New key" spellcheck="false" />
        <span class="equals" aria-hidden="true">=</span>
        <input class="mono" bind:value={newValue} placeholder="value" aria-label="New value" spellcheck="false" />
        <button type="submit" disabled={!newKey.trim()}>Add</button>
      </form>
    </div>
  </section>
</div>

<style>
  .properties {
    container-type: inline-size;
    display: grid;
    gap: 32px;
    max-width: 920px;
  }

  /* We keep the editor under the preview at vanilla's row width, so each edit lands where you're looking. */
  .listing {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 22px;
    max-width: 612px;
    padding-bottom: 32px;
    border-bottom: 1px solid var(--line);
  }

  .preview {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 10px;
  }

  .listing-fields {
    display: grid;
    gap: 20px;
    align-content: start;
  }

  .group {
    display: grid;
    gap: 14px;
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(200px, 1fr));
    gap: 16px 18px;
  }

  /* Text inputs keep their size=20 width unless we let them shrink, which overflowed the 200px columns. */
  .field {
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    gap: 6px;
    align-content: start;
    min-width: 0;
  }

  .field > label,
  .label,
  .short {
    color: var(--text);
    font-size: var(--step-0);
  }

  .short {
    max-width: 200px;
  }

  .group-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 12px;
  }

  .check-row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .reset {
    padding: 2px 6px;
    color: var(--ochre-ink);
  }

  .reset.unchanged {
    visibility: hidden;
  }

  .control {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .control > input,
  .control > select {
    flex: 1;
    min-width: 0;
  }

  /* Numbers stay short so their unit sits beside them, and a field with a hint takes two columns
     so the hint doesn't wrap into a sliver. */
  .control > input[type='number'] {
    flex: 0 1 9rem;
  }

  .grid > .field:has(.hint) {
    grid-column: span 2;
  }

  .unit {
    color: var(--muted);
    font-size: var(--step--1);
  }

  .mono,
  textarea {
    font-family: var(--font-log);
    font-size: var(--step--1);
  }

  .checks {
    display: grid;
    gap: 10px;
    max-width: 64ch;
  }

  .check {
    align-items: start;
  }

  .check input {
    margin-top: 0.3em;
  }

  .hint {
    color: var(--muted);
    font-size: var(--step--1);
  }

  .hint.indent {
    padding-left: 25px;
  }

  .warn {
    color: var(--warn);
    font-weight: 700;
  }

  .buttons {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .codes {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px 14px;
  }

  .code-group {
    display: flex;
    gap: 4px;
  }

  .codes button {
    height: 26px;
    min-width: 26px;
    padding: 0 6px;
    font-size: var(--step--1);
    font-weight: 400;
    line-height: 1;
  }

  .codes .swatch,
  .codes .swatch:hover:not(:disabled) {
    width: 22px;
    min-width: 22px;
    padding: 0;
    background: var(--swatch);
    border-color: var(--line-strong);
  }

  .codes .swatch:hover {
    outline: 2px solid var(--edge);
    outline-offset: 1px;
  }

  .style.bold {
    font-weight: 800;
  }

  .style.italic {
    font-style: italic;
  }

  .style.underline {
    text-decoration: underline;
  }

  .style.strikethrough {
    text-decoration: line-through;
  }

  .style.obfuscated,
  .placeholder {
    font-family: var(--font-log);
  }

  .others {
    display: grid;
    gap: 8px;
  }

  .pair {
    display: grid;
    grid-template-columns: minmax(0, 18rem) auto minmax(0, 1fr) 6rem;
    align-items: center;
    gap: 8px;
  }

  .equals {
    color: var(--muted);
    font-family: var(--font-log);
  }

  .pair button {
    justify-self: start;
  }
</style>
