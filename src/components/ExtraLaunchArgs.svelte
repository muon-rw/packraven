<script lang="ts" module>
  import type { RunArgs } from '../lib/api'

  export type RunArgsDraft = Record<keyof RunArgs, string>

  export const emptyDraft = (): RunArgsDraft => ({ clientJvmArgs: '', clientArgs: '', serverJvmArgs: '', serverArgs: '' })

  const lines = (text: string) =>
    text
      .split('\n')
      .map((line) => line.trim())
      .filter(Boolean)

  const active = (text: string) => lines(text).filter((line) => !line.startsWith('#'))

  export const activeLines = (args: RunArgs) => Object.values(args).reduce((count, list: string[]) => count + list.filter((line) => line.trim() && !line.trim().startsWith('#')).length, 0)

  export const toDraft = (args: RunArgs): RunArgsDraft => ({
    clientJvmArgs: args.clientJvmArgs.join('\n'),
    clientArgs: args.clientArgs.join('\n'),
    serverJvmArgs: args.serverJvmArgs.join('\n'),
    serverArgs: args.serverArgs.join('\n'),
  })

  export const fromDraft = (draft: RunArgsDraft): RunArgs => ({
    clientJvmArgs: lines(draft.clientJvmArgs),
    clientArgs: lines(draft.clientArgs),
    serverJvmArgs: lines(draft.serverJvmArgs),
    serverArgs: lines(draft.serverArgs),
  })

  // Mirrors split_args in launch.rs, so we check and show exactly what Java receives.
  function splitArgs(line: string) {
    const tokens: string[] = []
    let current = ''
    let quoted = false
    let hasToken = false
    for (const c of line) {
      if (c === '"') {
        quoted = !quoted
        hasToken = true
      } else if (/\s/.test(c) && !quoted) {
        if (hasToken) tokens.push(current)
        current = ''
        hasToken = false
      } else {
        current += c
        hasToken = true
      }
    }
    if (hasToken) tokens.push(current)
    return { tokens, unclosed: quoted }
  }

  // Java reads the next token as the value of these options, so it may not start with a dash.
  const TAKES_VALUE = new Set(['-cp', '-classpath', '--class-path', '-p', '--module-path', '--upgrade-module-path', '--add-modules', '--limit-modules', '--add-opens', '--add-exports', '--add-reads', '--patch-module', '--enable-native-access'])

  function checkJvm(text: string) {
    const tokens: string[] = []
    for (const line of active(text)) {
      const split = splitArgs(line)
      if (split.unclosed) return { tokens, problem: `This line has an opening double quote with no closing one: ${line}` }
      tokens.push(...split.tokens)
    }
    const stray = tokens.find((token, i) => !token.startsWith('-') && !TAKES_VALUE.has(tokens[i - 1]))
    return { tokens, problem: stray === undefined ? null : `"${stray}" doesn't start with a dash, so Java will treat it as the main class and the run won't start.` }
  }
</script>

<script lang="ts">
  import { onDestroy, type Snippet } from 'svelte'
  import { appSettingsView, nav } from '../lib/nav.svelte'

  let {
    draft = $bindable(),
    scope,
    inherited = 0,
    onappsettings,
    onedit,
    onchange,
    status,
  }: {
    draft: RunArgsDraft
    scope: 'app' | 'pack'
    inherited?: number
    onappsettings?: () => void
    onedit?: () => void
    onchange?: (key: keyof RunArgs) => void
    status?: Snippet<[keyof RunArgs]>
  } = $props()

  const fields = [
    { key: 'clientJvmArgs', label: 'Client JVM flags', placeholder: '-Dmixin.debug.export=true', jvm: true },
    { key: 'clientArgs', label: 'Client arguments', placeholder: '--width\n1920', jvm: false },
    { key: 'serverJvmArgs', label: 'Server JVM flags', placeholder: '-XX:+HeapDumpOnOutOfMemoryError', jvm: true },
    { key: 'serverArgs', label: 'Server arguments', placeholder: '--world\nDebug World', jvm: false },
  ] as const

  let checks = $derived(Object.fromEntries(fields.filter((f) => f.jvm).map((f) => [f.key, checkJvm(draft[f.key])])) as Partial<Record<keyof RunArgs, ReturnType<typeof checkJvm>>>)
  let problems = $derived(Object.values(checks).filter((check) => check.problem).length)
  let inUse = $derived(Object.values(draft).reduce((count, text) => count + active(text).length, 0))

  let open = $state(false)
  let details = $state<HTMLDetailsElement>()
  // A forgotten debugging flag changes every run, so we keep the fields in view whenever any are on.
  $effect(() => {
    if (inUse) open = true
  })

  const timers = new Map<keyof RunArgs, ReturnType<typeof setTimeout>>()

  function clearTimer(key: keyof RunArgs) {
    const timer = timers.get(key)
    if (timer !== undefined) {
      clearTimeout(timer)
      timers.delete(key)
    }
  }

  function scheduleChange(key: keyof RunArgs) {
    clearTimer(key)
    onedit?.()
    if (checks[key]?.problem) return
    timers.set(
      key,
      setTimeout(() => {
        timers.delete(key)
        if (!checks[key]?.problem) onchange?.(key)
      }, 400),
    )
  }

  function commitChange(key: keyof RunArgs) {
    clearTimer(key)
    if (checks[key]?.problem) return
    onchange?.(key)
  }

  onDestroy(() => {
    for (const key of [...timers.keys()]) commitChange(key)
  })
</script>

<details bind:open bind:this={details}>
  <summary>
    {scope === 'app' ? 'Extra launch arguments for every pack' : 'Extra launch arguments'}{#if inUse}{' '}<span class="muted">({inUse} {inUse === 1 ? 'line' : 'lines'} in use{#if problems}, <span class="warn">{problems === 1 ? '1 problem' : `${problems} problems`}</span>{/if})</span>{/if}
  </summary>
  <div class="body">
    <div class="hint">
      {#if scope === 'app'}
        <p>For debugging on this machine. packraven adds these to every pack's test runs, before each pack's own extra arguments from Pack settings, and never ships them in a server pack.</p>
      {:else}
        <p>
          For debugging this pack on this machine. packraven adds these to test runs after the pack's own flags and arguments{inherited ? ' and the ones for every pack' : ''},
          and never ships them in the server pack or saves them in the pack repo.
          {#if inherited}
            Test runs also get {inherited === 1 ? '1 line' : `${inherited} lines`} from <button type="button" class="link" onclick={() => { appSettingsView.tab = 'java'; nav.anchor = 'java-title'; onappsettings?.() }}>App settings</button>.
          {/if}
        </p>
      {/if}
      <p id="{scope}-args-syntax">
        packraven splits each JVM flag line at spaces, so put double quotes around a value that contains one, and passes each argument line whole. Java uses
        the last of repeated flags, so an -Xmx here overrides the memory setting. Start a line with # to turn it off. Each run's log lists what packraven added.
      </p>
    </div>
    <div class="fields">
      {#each fields as field (field.key)}
        {@const check = checks[field.key]}
        {@const id = `${scope}-${field.key}`}
        <div class="field">
          <div class="label-row">
            <label for={id}>{field.label}</label>
            {@render status?.(field.key)}
          </div>
          <textarea
            {id}
            rows={Math.min(8, Math.max(3, draft[field.key].split('\n').length))}
            wrap="off"
            bind:value={draft[field.key]}
            oninput={() => scheduleChange(field.key)}
            onchange={() => commitChange(field.key)}
            placeholder={field.placeholder}
            spellcheck="false"
            aria-invalid={!!check?.problem}
            aria-describedby={[check?.problem ? `${id}-problem` : check?.tokens.length ? `${id}-tokens` : null, `${scope}-args-syntax`].filter(Boolean).join(' ')}
          ></textarea>
          {#if check}
            <!-- We announce problems as you type, but not the echo, which changes on every key. -->
            <div aria-live="polite">
              {#if check.problem}<p class="warn" id="{id}-problem">{check.problem}</p>{/if}
            </div>
          {/if}
          {#if check && !check.problem && check.tokens.length}
            <p class="tokens" id="{id}-tokens">
              <span class="visually-hidden">Java gets these arguments:</span>
              <span aria-hidden="true">Java gets</span>
              {#each check.tokens as token, i (i)}<code>{token}</code>{/each}
            </p>
          {/if}
        </div>
      {/each}
    </div>
  </div>
</details>

<style>
  details {
    margin-top: 4px;
  }

  summary {
    width: fit-content;
    cursor: pointer;
    color: var(--text);
    font-size: var(--step-0);
  }

  .body {
    display: grid;
    gap: 14px;
    margin-top: 10px;
  }

  .hint {
    display: grid;
    gap: 6px;
    max-width: 72ch;
    color: var(--muted);
    font-size: var(--step--1);
  }

  .fields {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 14px 18px;
  }

  /* Margins rather than a gap, so the empty live region takes no space. */
  .field {
    display: grid;
    align-content: start;
    min-width: 0;
  }

  .label-row {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 6px;
  }

  label {
    color: var(--text);
    font-size: var(--step-0);
  }

  .warn,
  .tokens {
    margin-top: 6px;
  }

  textarea {
    font-family: var(--font-log);
    font-size: var(--step--1);
    overflow-x: auto;
  }

  .warn {
    color: var(--warn);
    font-size: var(--step--1);
    font-weight: 700;
  }

  .tokens {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px;
    color: var(--muted);
    font-size: var(--step--1);
  }

  .tokens code {
    padding: 0 4px;
    font-family: var(--font-log);
    /* break-all fills each row, where the default would break right after a flag's leading dash. */
    word-break: break-all;
    border: 1px solid var(--line);
    border-radius: var(--radius-control);
  }
</style>
