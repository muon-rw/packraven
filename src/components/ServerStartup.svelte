<script lang="ts">
  import ServerReadme from './ServerReadme.svelte'

  let {
    memory,
    jvmArgs,
    args,
    graalvm,
    bundleMods,
    minecraft,
    launch,
    testPort,
    savedCount,
    onchange,
    onfiles,
  }: {
    memory: string
    jvmArgs: string[]
    args: string[]
    graalvm: string
    bundleMods: boolean
    minecraft: string | null
    launch: { windows: string; unix: string } | null
    testPort: number
    savedCount: number
    onchange: (changes: { memory?: string; jvm_args?: string[]; args?: string[]; graalvm?: string; bundle_mods?: boolean }) => void
    onfiles: (path: string) => void
  } = $props()

  const modsOptions = [
    {
      bundle: false,
      label: 'Download on first start',
      hint: "The zip will stay very small. On server startup, we'll download each mod from CurseForge or Modrinth and check it's intact. We still put mods that aren't on either site in the zip.",
    },
    {
      bundle: true,
      label: 'Include them in the zip',
      hint: "The zip will include every jar file, to better support hosts who expect them. If the zip is over 1 GB, packraven can't upload it, so we'll open the Authors Portal for you.",
    },
  ]

  type GraalvmChoice = 'auto' | '21' | '25' | 'exact'

  let graalvmChoice = $state<GraalvmChoice>('auto')
  let exactGraalvm = $state('')
  $effect.pre(() => {
    const configured = graalvm.trim()
    graalvmChoice = configured === '' ? 'auto' : configured === '21' || configured === '25' ? configured : 'exact'
    if (graalvmChoice === 'exact') exactGraalvm = configured
  })

  let automaticGraalvm = $derived.by(() => {
    const [major, minor] = (minecraft ?? '').split('.').map(Number)
    if (major >= 26) return '25'
    if (major === 1 && minor >= 18) return '21'
    return null
  })
  let hostGraalvm = $derived(graalvmChoice === 'auto' ? automaticGraalvm : graalvmChoice === 'exact' ? exactGraalvm.trim() || null : graalvmChoice)

  function chooseGraalvm(choice: GraalvmChoice) {
    graalvmChoice = choice
    if (choice === 'auto') onchange({ graalvm: '' })
    else if (choice !== 'exact') onchange({ graalvm: choice })
    else if (validGraalvm(exactGraalvm)) onchange({ graalvm: exactGraalvm.trim() })
  }

  const validGraalvm = (text: string) => /^\d+\.\d+(\.\d+)?$/.test(text.trim()) && Number(text.split('.')[0]) >= 17
  const validMemory = (text: string) => /^\d+[mg]$/i.test(text.trim())

  let memoryDraft = $state('')
  $effect.pre(() => {
    memoryDraft = memory
  })
  let memoryInvalid = $derived(!validMemory(memoryDraft))

  function saveMemory() {
    if (!memoryInvalid && memoryDraft.trim() !== memory) onchange({ memory: memoryDraft.trim() })
  }

  const lines = (text: string) =>
    text
      .split('\n')
      .map((line) => line.trim())
      .filter(Boolean)

  let flagsDraft = $state('')
  let argsDraft = $state('')
  $effect.pre(() => {
    flagsDraft = jvmArgs.join('\n')
  })
  $effect.pre(() => {
    argsDraft = args.join('\n')
  })

  let flagProblem = $derived.by(() => {
    const flags = lines(flagsDraft)
    if (flags.some((flag) => /^-Xmx/i.test(flag))) return 'Set the heap size under Memory instead. packraven already writes -Xmx from it, and Java uses whichever comes last.'
    const stray = flags.find((flag) => !flag.startsWith('-') && !flag.startsWith('#'))
    if (stray) return `"${stray}" doesn't start with a dash, so Java will treat it as the main class and the server won't start.`
    return null
  })

  // We mirror the backend's quoting closely enough for a preview; each script quotes for its own shell.
  const plain = (arg: string) => /^[\w\-=.,:/@+]+$/.test(arg)
  let scripts = $derived([
    {
      file: 'run.bat',
      java: hostGraalvm ? `java\\graalvm-${hostGraalvm}\\bin\\java.exe` : 'java',
      launch: launch?.windows ?? '<loader>',
      args: lines(argsDraft).map((arg) => (plain(arg) ? arg : `"${arg.replaceAll('%', '%%')}"`)),
    },
    {
      file: 'run.sh',
      java: hostGraalvm ? `java/graalvm-${hostGraalvm}/bin/java` : 'java',
      launch: launch?.unix ?? '<loader>',
      args: lines(argsDraft).map((arg) => (plain(arg) ? arg : `'${arg}'`)),
    },
  ])
</script>

<div class="startup">
  <div class="form">
    <section aria-labelledby="mods-title">
      <h3 id="mods-title">Server Mod Files</h3>
      <div class="options" role="radiogroup" aria-labelledby="mods-title" aria-describedby="mods-hint">
        {#each modsOptions as option (option.bundle)}
          {@const id = `mods-${option.bundle ? 'bundled' : 'downloaded'}`}
          <!-- Clicking the hint picks its option, so it sits inside the label, and aria-labelledby keeps it out of the radio's name. -->
          <label class="option">
            <input type="radio" name="server-mods" checked={bundleMods === option.bundle} aria-labelledby="{id}-label" aria-describedby="{id}-hint" onchange={() => onchange({ bundle_mods: option.bundle })} />
            <span class="option-label" id="{id}-label">{option.label}{#if !option.bundle}{' '}<span class="muted">(recommended)</span>{/if}</span>
            <span class="hint" id="{id}-hint">{option.hint}</span>
          </label>
        {/each}
      </div>
      <p class="hint" id="mods-hint">We use this for the server packs you build from now on. Either way, we copy the mods on this computer into test runs.</p>
    </section>

    <section aria-labelledby="memory-title">
      <h3 id="memory-title">Memory</h3>
      <label class="field short">
        <span class="visually-hidden">Memory</span>
        <input
          bind:value={memoryDraft}
          placeholder="6G"
          spellcheck="false"
          aria-invalid={memoryInvalid}
          aria-describedby="memory-hint"
          onchange={saveMemory}
        />
      </label>
      <p class="hint" id="memory-hint" class:failed={memoryInvalid}>
        {memoryInvalid ? 'Enter a size in gigabytes or megabytes, such as 6G or 6144M.' : 'The most memory the server may use.'}{#if !memoryInvalid}{' '}Hosts can raise it in <span class="file">user_jvm_args.txt</span>.{/if}
      </p>
    </section>

    <section aria-labelledby="java-title">
      <h3 id="java-title">Java</h3>
      <div class="inline">
        <select value={graalvmChoice} onchange={(e) => chooseGraalvm(e.currentTarget.value as GraalvmChoice)} aria-label="Java the run scripts download" aria-describedby="java-hint">
          <option value="auto">Automatic ({automaticGraalvm ? `GraalVM ${automaticGraalvm}` : minecraft ? "the host's own Java" : 'GraalVM 21, or 25 for Minecraft 26.1 and later'})</option>
          <option value="21">Latest GraalVM 21</option>
          <option value="25">Latest GraalVM 25</option>
          <option value="exact">Exact GraalVM release</option>
        </select>
        {#if graalvmChoice === 'exact'}
          <input
            bind:value={exactGraalvm}
            placeholder="21.0.7"
            spellcheck="false"
            class="mono"
            aria-label="GraalVM release"
            aria-invalid={!validGraalvm(exactGraalvm)}
            aria-describedby="java-hint"
            onchange={() => validGraalvm(exactGraalvm) && onchange({ graalvm: exactGraalvm.trim() })}
          />
        {/if}
      </div>
      <p class="hint" id="java-hint">
        {#if graalvmChoice === 'exact'}<span class:failed={!validGraalvm(exactGraalvm)}>Enter a GraalVM release such as 21.0.7.</span>{/if}
        The run scripts download this on a host's first start. Latest means the newest release at that moment. Hosts can run their own Java instead by
        writing its path in <span class="file">java.txt</span> next to the scripts.
      </p>
    </section>

    <section aria-labelledby="flags-title">
      <h3 id="flags-title">JVM flags</h3>
      <textarea
        rows="5"
        bind:value={flagsDraft}
        placeholder={'-XX:+UseG1GC\n-Dfml.readTimeout=180'}
        spellcheck="false"
        aria-labelledby="flags-title"
        aria-describedby="flags-hint"
        aria-invalid={!!flagProblem}
        onchange={() => onchange({ jvm_args: lines(flagsDraft) })}
      ></textarea>
      <p class="hint" id="flags-hint">
        {#if flagProblem}<span class="warn">{flagProblem}</span>{/if}
        One per line. packraven adds them to <span class="file">user_jvm_args.txt</span> after the loader installer's own, for hosts and test runs alike.
      </p>
    </section>

    <section aria-labelledby="args-title">
      <h3 id="args-title">Server arguments</h3>
      <textarea
        rows="3"
        bind:value={argsDraft}
        placeholder="--universe&#10;worlds"
        spellcheck="false"
        aria-labelledby="args-title"
        aria-describedby="args-hint"
        onchange={() => onchange({ args: lines(argsDraft) })}
      ></textarea>
      <p class="hint" id="args-hint">One per line, so an argument with spaces stays whole. packraven passes them to the server after nogui.</p>
    </section>
  </div>

  <aside class="result" aria-labelledby="result-title">
    <h3 id="result-title">Files added to the Server Pack</h3>
    <figure>
      <figcaption class="file">user_jvm_args.txt</figcaption>
      <pre><span class="muted"># The loader installer's lines</span>
-Xmx{memoryInvalid ? memory : memoryDraft.trim()}{#each lines(flagsDraft) as flag, i (i)}{'\n'}{flag}{/each}</pre>
    </figure>
    {#each scripts as script (script.file)}
      <figure>
        <figcaption class="file">{script.file}</figcaption>
        <pre>{[script.java, '@user_jvm_args.txt', script.launch, 'nogui', ...script.args].join(' ')}</pre>
      </figure>
    {/each}
    <p class="hint">
      Test runs use the same flags and arguments with this machine's Java, on port {testPort}, open only to this computer. They also get any extra launch
      arguments you set for debugging in App settings or Pack settings.
    </p>
    <ServerReadme onedit={onfiles} {savedCount} />
  </aside>
</div>

<style>
  .startup {
    display: grid;
    gap: 32px;
    max-width: 1080px;
  }

  @container (min-width: 820px) {
    .startup {
      grid-template-columns: minmax(0, 1fr) minmax(18rem, 26rem);
    }

    .result {
      position: sticky;
      top: 0;
    }
  }

  .form {
    display: grid;
    gap: 28px;
    align-content: start;
  }

  section {
    display: grid;
    gap: 10px;
  }

  .short {
    max-width: 200px;
  }

  .options {
    display: grid;
    gap: 10px;
  }

  .option {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    column-gap: 8px;
    row-gap: 2px;
    align-items: baseline;
    max-width: 70ch;
    cursor: pointer;
  }

  .option input {
    margin: 0;
  }

  .option .hint {
    grid-column: 2;
  }

  .inline {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .inline select {
    flex: 1 1 18rem;
  }

  .inline input {
    flex: 0 1 10rem;
  }

  .mono,
  textarea,
  pre {
    font-family: var(--font-log);
    font-size: var(--step--1);
    font-variant-numeric: tabular-nums;
  }

  .hint {
    max-width: 75ch;
    color: var(--muted);
    font-size: var(--step--1);
  }

  .hint.failed,
  .hint .failed {
    color: var(--fail);
  }

  .warn {
    display: block;
    margin-bottom: 2px;
    color: var(--warn);
    font-weight: 700;
  }

  .result {
    display: grid;
    gap: 14px;
    align-content: start;
  }

  figure {
    display: grid;
    gap: 4px;
    margin: 0;
  }

  figcaption {
    color: var(--muted);
  }

  pre {
    margin: 0;
    padding: 10px 12px;
    overflow-x: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: var(--radius-control);
  }
</style>
