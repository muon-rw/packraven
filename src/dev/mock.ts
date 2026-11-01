// Browser-only stand-in for the Tauri backend, so the UI can run in a plain browser during development.
// We load data from src/dev/fixtures.json when present (gitignored; you generate it from a real instance),
// and otherwise from the committed demo-fixtures.json, a snapshot of a real pack.
import demoFixtures from './demo-fixtures.json'
import type { PendingStep, Platform, ReleaseStatus, ServerPackUpload } from '../lib/api'

// Add `?scenario=` to the URL to put the pack in one of the states we review the UI in.
const scenarios = {
  dirty: 'Uncommitted changes, one commit to push, and a jar that blocks publishing',
  ready: 'A clean tree with mod changes since the last release and nothing blocking a publish',
  empty: 'A pack you just added, with no commits, releases or release notes',
  nopack: 'No packs added yet, so the app opens on Add pack',
  newerlock: "The dirty tree, but a newer packraven wrote mods.lock.json, so the pack won't load",
}
type Scenario = keyof typeof scenarios
const requested = new URLSearchParams(location.search).get('scenario')
const scenario: Scenario = requested && requested in scenarios ? (requested as Scenario) : 'dirty'

type Fixtures = {
  instance: string
  version: string
  minecraft: string
  loader: string
  lockCount: number
  dirs: Record<string, unknown[]>
  files: Record<string, unknown>
  status: { staged: { path: string; status: string }[]; unstaged: { path: string; status: string }[] } & Record<string, unknown>
  commits: unknown[]
  diffs: Record<string, unknown>
  changed: string[]
}

const found = import.meta.glob('./fixtures.json', { eager: true, import: 'default' }) as Record<string, Fixtures>
const fx: Fixtures = found['./fixtures.json'] ?? (demoFixtures as Fixtures)
if (scenario !== 'dirty' && scenario !== 'newerlock') {
  fx.status.staged = []
  fx.status.unstaged = []
  fx.changed = []
}
if (scenario === 'empty') {
  fx.commits = []
  fx.lockCount = 0
}

const staged = fx.status.staged.length ? fx.status.staged : fx.status.unstaged.slice(0, 2)
const unstaged = fx.status.staged.length ? fx.status.unstaged : fx.status.unstaged.slice(2)
// JEI backups from before the pack's .gitignore covered *.bak.*, so the Git view can offer to stop tracking them.
let trackedIgnored =
  scenario === 'dirty'
    ? ['config/jei/client/jei-client.ini.bak.1', 'config/jei/client/jei-client.ini.bak.2', 'config/jei/jei-colors.ini.bak.1', 'config/jei/recipe-category-sort-order.ini.bak.1']
    : []

const gitStatus = () => ({ branch: 'master', upstream: 'origin/master', ahead: 1, behind: 0, hasCommits: true, hasRemote: true, conflicted: [], ...fx.status, trackedIgnored, staged, unstaged })

// Texture changes, so the dirty scenario covers the side-by-side image diff.
if (scenario === 'dirty') {
  const texture = (path: string) => ({ text: `diff --git a/${path} b/${path}\nindex 3f2a1b0..9c8d7e6 100644\nBinary files a/${path} and b/${path} differ\n`, binary: true, truncated: false })
  unstaged.push({ path: 'kubejs/assets/ravens/textures/item/raven_feather.png', status: 'M' })
  staged.push({ path: 'kubejs/assets/ravens/textures/block/raven_banner.png', status: 'A' })
  fx.diffs['u:kubejs/assets/ravens/textures/item/raven_feather.png'] = texture('kubejs/assets/ravens/textures/item/raven_feather.png')
  fx.diffs['s:kubejs/assets/ravens/textures/block/raven_banner.png'] = texture('kubejs/assets/ravens/textures/block/raven_banner.png')
}
const diffFor = (path: string, isStaged: boolean) => fx.diffs[`${isStaged ? 's' : 'u'}:${path}`] ?? fx.diffs[`u:${path}`] ?? { text: '', binary: false, truncated: false }

const pack = {
  dir: fx.instance,
  name: "Raven's High Fantasy",
  minecraft: fx.minecraft,
  loader: fx.loader,
  addonCount: fx.lockCount,
  hasGit: true,
  hasLock: true,
  missingMods: 0,
  hooks: 'current',
  trackedBinaries: [],
  remote: 'https://github.com/muon-rw/Ravens-High-Fantasy.git',
  added: true,
}

// A second pack you can switch to. The mock serves the first pack's data for it under its own name.
const otherPack = { ...pack, dir: 'C:/Instances/Otherworld', name: 'Otherworld', remote: 'https://github.com/muon-rw/Otherworld.git' }
let activePack = pack.dir

const config = {
  author: 'muon-rw',
  curseforge: { project_id: scenario === 'empty' ? 0 : 1234567, release_type: 'beta', manual_release: false, allowed_override_jars: [] as string[], exclude: [] as string[] },
  // In the dirty scenario, you included an older Prominent-GLOBAL before updating it.
  modrinth: { project_id: scenario === 'empty' ? '' : 'ravens-high-fantasy', allowed_override_jars: scenario === 'dirty' ? ['mods/Prominent-GLOBAL-MC1.21.1-4.1.4.jar'] : [], exclude: [] as string[], allowed_authors: [] as string[] },
  export: { exclude: [] },
  server: {
    exclude: [],
    client_only: [],
    keep: [],
    memory: '6G',
    jvm_args: scenario === 'empty' ? [] : ['-XX:+UseG1GC', '-Dfml.readTimeout=180'],
    args: [] as string[],
    graalvm: '',
    properties: (scenario === 'empty'
      ? { 'allow-flight': 'true', 'max-tick-time': '60000', difficulty: 'normal' }
      : { motd: '§6{name} §e{version}§r\\n§7Dragons, dungeons and §aspells', 'allow-flight': 'true', difficulty: 'hard', 'max-players': '12', 'view-distance': '12', 'max-tick-time': '-1', 'pause-when-empty-seconds': '0' }) as Record<string, string>,
  },
  test: { port: 25565, client_memory: '8G' },
  discord: { role_id: '112233445566778899', color: '#4d6b2f', announce_on_approval: true, title: '{project_name} - v{version}', show_author: true, header: '', footer: '[Install]({file_url})' },
  updates: { channel: 'alpha', locked: [] as string[] },
}

const modrinthRelease = (version: string) => ({ status: 'Listed', url: `https://modrinth.com/project/ravens-high-fantasy/version/${version}` })
const zipStem = (version: string) => `${pack.name.replace(/[^\w -]/g, '').trim().replace(/ /g, '-')}-${version}`
// packraven rebuilds a server pack CurseForge turned down as too large before it tries again.
const tooLarge = "CurseForge's upload API turned down the 1.29 GB file as too large"
const unsentServerPack = (version: string, fileId: number, error = tooLarge, packChanged = false): ServerPackUpload => ({
  zip: `C:/Users/you/AppData/Roaming/dev.muon.packraven/packs/ravens-high-fantasy/builds/${version}/${zipStem(version)}-server.zip`,
  uploadPage: `https://authors.curseforge.com/#/projects/1234567/files/${fileId}/create-additional-file`,
  displayName: `Server Pack ${version}`,
  fileName: `${zipStem(version)}-server.zip`,
  error,
  retryable: true,
  rebuild: error === tooLarge,
  packChanged,
  tag: `v${version}`,
})
// Add `serverpackfails` to the URL to have a mock publish fail its server pack upload.
const serverPackFails = new URLSearchParams(location.search).has('serverpackfails')
// The newest release missed Modrinth and its server pack in the dirty scenario, so Releases offers to upload them.
const releases: ReleaseStatus[] = scenario === 'empty' ? [] : [
  { version: '0.1.0', publishedAt: '2026-10-03T18:20:00Z', announce: true, announcedAt: '2026-10-03T19:02:00Z', displayName: `${pack.name} 0.1.0`, releaseType: 'beta', modrinth: modrinthRelease('0.1.0'), modrinthMissing: null, serverPackMissing: null,
    curseforge: { projectId: 1234567, fileId: 7001001, status: 'Approved', serverStatus: 'Approved', serverPackLinked: true } },
  { version: fx.version === '0.1.0' ? '0.1.1' : fx.version, publishedAt: '2026-10-12T21:40:00Z', announce: true, announcedAt: null,
    displayName: `${pack.name} 0.1.1`, releaseType: 'beta', modrinth: scenario === 'dirty' ? null : modrinthRelease('0.1.1'), modrinthMissing: scenario === 'dirty' ? `${pack.name.replace(/[^\w -]/g, '').trim().replace(/ /g, '-')}-0.1.1.mrpack` : null,
    serverPackMissing: scenario === 'dirty' ? unsentServerPack('0.1.1', 7105511, tooLarge, true) : null,
    curseforge: { projectId: 1234567, fileId: 7105511, status: 'Under review', serverStatus: scenario === 'dirty' ? 'Not uploaded' : 'Under review', serverPackLinked: null } },
]

// In the dirty scenario the newest release also stopped before its commit, tag and push, and the mods changed since.
const unfinishedSteps = new Map<string, { pending: PendingStep[]; packChanged: boolean }>(
  scenario === 'dirty' ? [[releases[1].version, { pending: [{ step: 'tag', message: `Release ${releases[1].version}` }, { step: 'push' }], packChanged: true }]] : [],
)

// Files a platform doesn't host, which a dual pack has to ship there or keep off it.
type MockSkew = { link: { projectId: string; title: string; url: string; evidence: 'sameFiles' | 'sameSource' | 'sameAuthor' }; shared: { fileId: number; version: string; newer: boolean; identical: boolean } | null; modrinthNewest: string | null; modrinthNewer: boolean; newestOnCurseforge: boolean; modrinthBuild: { version: string; differences: string[] } | null }
type MockCandidate = { name: string | null; url: string | null; authors: string[]; license: string | null; openLicense: boolean; allowlisted: boolean; known: boolean; skew: MockSkew | null; versionsKnown: boolean; locked: boolean }
const candidate = (fields: Partial<MockCandidate>): MockCandidate => ({ name: null, url: null, authors: [], license: null, openLicense: false, allowlisted: false, known: true, skew: null, versionsKnown: true, locked: false, ...fields })
const onModrinthAs = (title: string, evidence: MockSkew['link']['evidence'], rest: Omit<MockSkew, 'link'>): MockSkew => ({ link: { projectId: title.slice(0, 8), title, url: `https://modrinth.com/project/${title.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`, evidence }, ...rest })
const candidates: Record<Platform, Record<string, MockCandidate>> = {
  modrinth: scenario === 'dirty' ? {
    'mods/alltheleaks-1.1.13+1.21.1-neoforge.jar': candidate({ name: 'AllTheLeaks', url: 'https://www.curseforge.com/minecraft/mc-mods/alltheleaks', authors: ['Uncandango'], license: 'MIT License', openLicense: true }),
    'mods/framework-neoforge-1.21.1-0.13.11.jar': candidate({ name: 'Framework', url: 'https://www.curseforge.com/minecraft/mc-mods/framework', authors: ['MrCrayfish'], license: 'All Rights Reserved',
      skew: onModrinthAs('Framework', 'sameFiles', { shared: { fileId: 7200001, version: '0.13.12', newer: true, identical: true }, modrinthNewest: '0.13.12', modrinthNewer: true, newestOnCurseforge: true, modrinthBuild: null }) }),
    // The real Derelict 2.1.0 builds differ only in how one PNG is encoded, which we now count as the
    // same build. This row stands in for builds that really differ.
    'mods/derelict-2.1.0.jar': candidate({ name: "Mim1q's Derelict", url: 'https://www.curseforge.com/minecraft/mc-mods/derelict', authors: ['Mim1q'], license: 'All Rights Reserved',
      skew: onModrinthAs("Mim1q's Derelict", 'sameFiles', { shared: { fileId: 5848521, version: '2.0.0', newer: false, identical: false }, modrinthNewest: '2.1.0', modrinthNewer: false, newestOnCurseforge: true,
        modrinthBuild: { version: '2.1.0', differences: ['assets/derelict/lang/en_us.json', 'assets/derelict/textures/block/fancy_cobweb.png', 'com/github/mim1q/derelict/init/ModBlocks.class', 'META-INF/jars/gimm1q-0.6.1+1.20.1.jar!/fabric.mod.json'] } }) }),
    'mods/ftb-library-neoforge-2101.1.39.jar': candidate({ name: 'FTB Library (NeoForge)', url: 'https://www.curseforge.com/minecraft/mc-mods/ftb-library-forge', authors: ['FTB'], license: 'All Rights Reserved' }),
    'mods/overgrowns-origins-1.4.2.jar': candidate({ name: "Overgrown's Origins", url: 'https://www.curseforge.com/minecraft/mc-mods/overgrowns-origins', authors: ['Overgrown'], license: 'All Rights Reserved', locked: true,
      skew: onModrinthAs("Overgrown's Origins", 'sameSource', { shared: null, modrinthNewest: '2.0.1', modrinthNewer: true, newestOnCurseforge: false, modrinthBuild: null }) }),
    'resourcepacks/FreshAnimations-PT-BR.zip': candidate({}),
    'mods/Prominent-GLOBAL-MC1.21.1-4.1.5.jar': candidate({}),
  } : {},
  curseforge: scenario === 'dirty' ? {
    'mods/sodium-extra-neoforge-0.6.6+mc1.21.1.jar': candidate({ name: 'Sodium Extra', url: 'https://modrinth.com/project/sodium-extra', authors: ['FlashyReese'], license: 'GNU Lesser General Public License v3.0 only', openLicense: true, allowlisted: true }),
    'mods/ravens-patches-1.0.jar': candidate({}),
  } : {},
}
const usingModrinthBuild = new Set<string>()
const choicesOf = (platform: Platform) => (platform === 'curseforge' ? config.curseforge : config.modrinth)
const permittedAuthors = (platform: Platform): string[] => (platform === 'modrinth' ? config.modrinth.allowed_authors : [])
// export::choice_covers and export::any_version_pattern, where only * is a wildcard.
const escapeRegex = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
const covers = (choice: string, path: string) => choice === path || (choice.includes('*') && new RegExp('^' + choice.split('*').map(escapeRegex).join('.*') + '$').test(path))
const anyVersion = (path: string) => {
  const slash = path.lastIndexOf('/')
  const name = path.slice(slash + 1)
  const dot = name.lastIndexOf('.')
  if (dot < 0) return null
  const stem = name.slice(0, dot)
  const byPart = [...stem.matchAll(/[-_+ ]/g)].map((m) => m.index! + 1).find((start) => /\d/.test(stem.slice(start).split(/[-_+ ]/)[0]))
  const firstPart = stem.split(/[-_+ ]/)[0]
  const byDigit = /^[a-z]/i.test(firstPart) ? firstPart.search(/\d/) : -1
  const cut = byPart ?? (byDigit > 0 ? byDigit : !/\d/.test(stem) && /[a-z]/i.test(stem) ? stem.length : undefined)
  return cut === undefined ? null : `${path.slice(0, slash + 1)}${stem.slice(0, cut)}*${name.slice(dot)}`
}
const isChosen = (platform: Platform, path: string) => [...choicesOf(platform).allowed_override_jars, ...choicesOf(platform).exclude].some((choice) => covers(choice, path))
const earlierChoice = (platform: Platform, path: string) => {
  const pattern = anyVersion(path)
  if (!pattern) return null
  const earlier = (list: string[], choice: 'ship' | 'leaveOut') => list.filter((entry) => entry !== path && !entry.includes('*') && covers(pattern, entry)).map((entry) => ({ path: entry, choice }))[0]
  return earlier(choicesOf(platform).allowed_override_jars, 'ship') ?? earlier(choicesOf(platform).exclude, 'leaveOut') ?? null
}
const fileView = (platform: Platform, path: string, author: string | null = null) => {
  const matches = path.includes('*') ? Object.keys(candidates[platform]).filter((file) => covers(path, file)).sort() : []
  const known = candidates[platform][matches[0] ?? path]
  return { path, name: known?.name ?? null, license: known?.license ?? null, openLicense: known?.openLicense ?? false, allowlisted: known?.allowlisted ?? false, author, anyVersion: path.includes('*') ? null : anyVersion(path), matches }
}
const overrideChoices = (platform: Platform) => ({
  shipped: choicesOf(platform).allowed_override_jars.map((path) => fileView(platform, path)),
  byAuthor: permittedAuthors(platform).map((author) => ({
    author,
    files: Object.keys(candidates[platform])
      .filter((path) => candidates[platform][path].authors.includes(author) && !isChosen(platform, path))
      .map((path) => fileView(platform, path, author)),
  })),
  keptOff: choicesOf(platform).exclude.map((path) => fileView(platform, path)),
})
const notOn = (platform: Platform) =>
  Object.keys(candidates[platform]).filter(
    (path) =>
      !isChosen(platform, path) &&
      !(platform === 'modrinth' && usingModrinthBuild.has(path)) &&
      !candidates[platform][path].authors.some((author) => permittedAuthors(platform).includes(author)),
  )
// Like export::override_files_for, the files in the pack that a choice covers, not the choices.
const decidedFiles = (platform: Platform, list: 'allowed_override_jars' | 'exclude') =>
  Object.keys(candidates[platform]).filter((path) => choicesOf(platform)[list].some((choice) => covers(choice, path))).map((path) => fileView(platform, path))
const candidateView = (platform: Platform, path: string) => ({ path, ...candidates[platform][path], anyVersion: anyVersion(path), earlier: earlierChoice(platform, path) })

const modChange = (name: string, from: string | null, to: string | null) => ({ name, url: `https://www.curseforge.com/minecraft/mc-mods/${name.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`, from, to, inModList: true })
const modDiff = {
  since: scenario === 'empty' ? null : 'v0.1.0',
  baseline: false,
  added: scenario === 'empty' ? [] : [modChange('Iron\'s Spells', null, null)],
  updated: scenario === 'empty' ? [] : [modChange('Sodium', 'sodium-neoforge-0.6.13.jar', 'sodium-neoforge-0.6.15.jar'), modChange('Apotheosis', 'Apotheosis-1.21.1-8.1.1.jar', 'Apotheosis-1.21.1-8.1.2.jar')],
  removed: scenario === 'empty' ? [] : [modChange('Better Clouds', null, null)],
}

const notes = scenario === 'empty' ? '' : '## Highlights\n\n- New dragon fights in the End\n- Rebalanced early armor tiers\n'

// Mirrors changelog::render in the backend.
function renderChangelog() {
  type Change = (typeof modDiff.added)[number]
  const link = (c: Change) => (c.url ? `[${c.name}](${c.url})` : c.name)
  const sections = [
    modDiff.added.length ? `\n### Added\n${modDiff.added.map((c) => `- ${link(c)}\n`).join('')}` : '',
    modDiff.updated.length ? `\n### Updated\n${modDiff.updated.map((c) => `- ${link(c)}: ${c.from} to ${c.to}\n`).join('')}` : '',
    modDiff.removed.length ? `\n### Removed\n${modDiff.removed.map((c) => `- ${link(c)}\n`).join('')}` : '',
  ].join('')
  if (!sections) return notes.trimEnd()
  return `${notes.trimEnd()}${notes.trim() ? '\n\n' : ''}## Mod changes\n${sections}`
}

const serverSides: { path: string; key: string | null; name: string; url: string | null; side: string; reason: string; override: string; requiredBy: string[] }[] =
  scenario === 'empty'
    ? []
    : [
        { path: 'mods/sodium-neoforge-0.6.15.jar', key: 'cf:394468', name: 'Sodium', url: 'https://www.curseforge.com/minecraft/mc-mods/sodium', side: 'client', reason: 'Modrinth: server unsupported', override: 'auto', requiredBy: [] },
        { path: 'mods/iris-neoforge-1.8.1.jar', key: 'cf:455508', name: 'Iris Shaders', url: 'https://www.curseforge.com/minecraft/mc-mods/irisshaders', side: 'client', reason: 'Modrinth: server unsupported', override: 'auto', requiredBy: [] },
        { path: 'mods/shouldersurfing-neoforge-4.5.0.jar', key: 'cf:243190', name: 'Shoulder Surfing Reloaded', url: null, side: 'client', reason: 'Marked client-only', override: 'client', requiredBy: [] },
        { path: 'mods/Apotheosis-1.21.1-8.1.2.jar', key: 'cf:313970', name: 'Apotheosis', url: null, side: 'both', reason: 'CurseForge: tagged Client and Server', override: 'auto', requiredBy: [] },
        { path: 'mods/athena-neoforge-1.21-4.0.2.jar', key: 'cf:841890', name: 'Athena', url: null, side: 'both', reason: 'Required by Chipped, which is needed on the server (Modrinth: server unsupported)', override: 'auto', requiredBy: ['Chipped'] },
        { path: 'mods/chipped-neoforge-1.21.1-4.0.2.jar', key: 'cf:456956', name: 'Chipped', url: null, side: 'both', reason: 'Modrinth: runs on both', override: 'auto', requiredBy: [] },
        {
          path: 'mods/sliblib-neoforge-1.21.1-3.0.1.jar',
          key: 'cf:1173405',
          name: 'S-Lib',
          url: null,
          side: 'both',
          reason: 'Required by Mutant Skeleton Armor Tweaks, Quests & Teams Fixes and 2 other server mods (Modrinth: server unsupported)',
          override: 'auto',
          requiredBy: ['Mutant Skeleton Armor Tweaks', 'Quests & Teams Fixes', 'Simply Bows', 'Simply Swords'],
        },
        { path: 'mods/simplytooltips-neoforge-1.21.1-0.1.5.jar', key: 'cf:1035547', name: 'Simply Tooltips', url: null, side: 'client', reason: "Marked client-only, but Simply Bows and Simply Swords require it, so the server won't start", override: 'client', requiredBy: ['Simply Bows', 'Simply Swords'] },
      ]

// A logo drawn inline, so the view never waits on CurseForge's image CDN.
const logo = (letter: string, color: string) =>
  `data:image/svg+xml,${encodeURIComponent(`<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 28 28"><rect width="28" height="28" rx="4" fill="${color}"/><text x="14" y="19" font-family="sans-serif" font-size="14" font-weight="700" fill="#fff" text-anchor="middle">${letter}</text></svg>`)}`

const authorsOf: Record<number, string[]> = {
  313970: ['Shadows_of_Fire'],
  283644: ['Shadows_of_Fire'],
  394468: ['jellysquid3', 'IMS'],
  455508: ['coderbot', 'IMS'],
  855414: ['iron431'],
  238222: ['mezz'],
  243190: ['Exopandora'],
  438180: ['Qendolin'],
  453763: ['FreshLX'],
  627557: ['EminGT'],
  328085: ['simibubi'],
  688231: ['IThundxr', 'Slimeist'],
  486392: ['Jozufozu'],
}

type MockMod = { path: string; projectId: number; fileId: number; name: string; url: string | null; logo: string; releaseType: string; requires: number[]; newer: { fileId: number; fileName: string; releaseType: string; fileDate: null }[] }
const newer = (fileId: number, fileName: string, releaseType = 'release') => ({ fileId, fileName, releaseType, fileDate: null })
const cfUrl = (slug: string) => `https://www.curseforge.com/minecraft/mc-mods/${slug}`
let installedMods: MockMod[] =
  scenario === 'empty'
    ? []
    : [
        { path: 'mods/Apotheosis-1.21.1-8.1.2.jar', projectId: 313970, fileId: 8826922, name: 'Apotheosis', url: cfUrl('apotheosis'), logo: logo('A', '#7a4b2a'), releaseType: 'release', requires: [283644], newer: [newer(8993983, 'Apotheosis-1.21.1-8.9.0.jar')] },
        { path: 'mods/Placebo-1.21.1-9.9.1.jar', projectId: 283644, fileId: 8800001, name: 'Placebo', url: cfUrl('placebo'), logo: logo('P', '#5b6b8a'), releaseType: 'release', requires: [], newer: [] },
        { path: 'mods/sodium-neoforge-0.6.15.jar', projectId: 394468, fileId: 8591792, name: 'Sodium', url: cfUrl('sodium'), logo: logo('S', '#3f6e4e'), releaseType: 'beta', requires: [], newer: [newer(8756580, 'sodium-neoforge-0.8.13+mc1.21.1.jar')] },
        { path: 'mods/iris-neoforge-1.8.1.jar', projectId: 455508, fileId: 8700000, name: 'Iris Shaders', url: cfUrl('irisshaders'), logo: logo('I', '#6a3f8a'), releaseType: 'release', requires: [394468], newer: [] },
        ...(scenario === 'dirty'
          ? [{ path: 'mods/iris-neoforge-1.8.0.jar', projectId: 455508, fileId: 8600500, name: 'Iris Shaders', url: cfUrl('irisshaders'), logo: logo('I', '#6a3f8a'), releaseType: 'release', requires: [394468], newer: [] }]
          : []),
        { path: 'mods/irons_spellbooks-1.21.1-3.4.0.jar', projectId: 855414, fileId: 8600000, name: "Iron's Spells 'n Spellbooks", url: cfUrl('irons-spells-n-spellbooks'), logo: logo('I', '#8a3f3f'), releaseType: 'release', requires: [313970], newer: [newer(8990000, 'irons_spellbooks-1.21.1-3.5.0-beta.2.jar', 'beta'), newer(8950001, 'irons_spellbooks-1.21.1-3.4.2.jar')] },
        { path: 'mods/jei-1.21.1-neoforge-19.21.0.247.jar', projectId: 238222, fileId: 8650000, name: 'Just Enough Items (JEI)', url: cfUrl('jei'), logo: logo('J', '#4a6a2a'), releaseType: 'release', requires: [], newer: [] },
        { path: 'mods/shouldersurfing-neoforge-4.5.0.jar', projectId: 243190, fileId: 8500000, name: 'Shoulder Surfing Reloaded', url: null, logo: logo('S', '#2a5a6a'), releaseType: 'release', requires: [], newer: [] },
        { path: 'mods/betterclouds-neoforge-1.3.2.jar.disabled', projectId: 438180, fileId: 8400000, name: 'Better Clouds', url: cfUrl('better-clouds'), logo: logo('B', '#5a7a9a'), releaseType: 'release', requires: [], newer: [newer(8800500, 'betterclouds-neoforge-1.3.4.jar')] },
        { path: 'resourcepacks/FreshAnimations_v1.9.4.zip', projectId: 453763, fileId: 8300000, name: 'Fresh Animations', url: cfUrl('fresh-animations'), logo: logo('F', '#9a6a2a'), releaseType: 'release', requires: [], newer: [] },
        { path: 'shaderpacks/ComplementaryReimagined_r5.5.1.zip', projectId: 627557, fileId: 8200000, name: 'Complementary Shaders - Reimagined', url: cfUrl('complementary-reimagined'), logo: logo('C', '#2a4a7a'), releaseType: 'release', requires: [], newer: [newer(8950000, 'ComplementaryReimagined_r5.6.zip')] },
      ]

// The empty scenario is a fresh pack without Better Compatibility Checker, so it has no version yet.
// Ready has bumped past the last release; dirty still reports it, so publishing would reuse the tag.
const bcc = { installed: scenario !== 'empty', version: scenario === 'empty' ? null : scenario === 'ready' ? '0.2.0' : (fx.version as string | null) }

// A copy of version_problem in pack.rs.
function versionProblem(raw: string) {
  const version = raw.trim()
  if (!version) return 'Enter a version.'
  if (/[\s\p{Cc}]/u.test(version)) return "Versions can't contain spaces."
  if (/[~^:?*[\\"<>|/]/.test(version)) return 'Versions can\'t contain any of ~ ^ : ? * [ \\ " < > | /.'
  if (version.includes('..') || version.includes('@{')) return "Versions can't contain .. or @{."
  if (version.endsWith('.') || version.endsWith('.lock')) return "Versions can't end in . or .lock."
  return null
}

function mockJob(job: string, lines: string[], message: string, change: () => void) {
  emit('job-state', { job, state: 'running', message: null })
  lines.forEach((text, i) => setTimeout(() => emit('job-log', { job, stream: 'info', text }), 150 * (i + 1)))
  setTimeout(() => {
    change()
    emit('job-state', { job, state: 'done', message })
  }, 150 * (lines.length + 2))
}

// Mirrors Jobs::enqueue: a change you ask for mid-run joins the run, which ends once none are left.
const modChanges: { id: number; lines: string[]; message: string; change: () => void }[] = []
let modsRunning = false
let nextChangeId = 1

function queueModChange(lines: string[], message: string, change: () => void): number {
  const id = nextChangeId++
  modChanges.push({ id, lines, message, change })
  if (modsRunning) return id
  modsRunning = true
  emit('job-state', { job: 'mods', state: 'running', message: null })
  const messages: string[] = []
  const next = () => {
    const item = modChanges.shift()
    if (!item) {
      modsRunning = false
      emit('job-state', { job: 'mods', state: 'done', message: messages.join(' ') })
      return
    }
    emit('job-item', { job: 'mods', id: item.id, state: 'started' })
    item.lines.forEach((text, i) => setTimeout(() => emit('job-log', { job: 'mods', stream: 'info', text }), 150 * (i + 1)))
    setTimeout(() => {
      item.change()
      emit('job-item', { job: 'mods', id: item.id, state: 'finished' })
      if (item.message) messages.push(item.message)
      next()
    }, 150 * (item.lines.length + 2))
  }
  next()
  return id
}

function cancelModChange(id: number): boolean {
  const index = modChanges.findIndex((c) => c.id === id)
  if (index >= 0) modChanges.splice(index, 1)
  return index >= 0
}

type MockProject = { projectId: number; name: string; summary: string; authors: string[]; downloads: number; updated: string; kind: string; fileName: string | null; releaseType?: string; requires?: number[] }
// prettier-ignore
const catalog: MockProject[] = [
  { projectId: 313970, name: 'Apotheosis', summary: 'All things that should have been.', authors: ['Shadows_of_Fire'], downloads: 135562157, updated: '2026-10-04T05:31:43Z', kind: 'mods', fileName: 'Apotheosis-1.21.1-8.9.0.jar', requires: [283644] },
  { projectId: 328085, name: 'Create', summary: 'Aesthetic technology that empowers the player.', authors: ['simibubi'], downloads: 162400000, updated: '2026-09-30T12:00:00Z', kind: 'mods', fileName: 'create-1.21.1-6.0.6.jar', requires: [486392] },
  { projectId: 688231, name: "Create: Steam 'n' Rails", summary: 'Adding depth to Create\'s rail network & steam system.', authors: ['IThundxr', 'Slimeist'], downloads: 41800000, updated: '2026-09-18T09:00:00Z', kind: 'mods', fileName: 'Steam_Rails-1.7.0+neoforge-mc1.21.1.jar', requires: [328085] },
  { projectId: 486392, name: 'Flywheel', summary: 'An overhauled rendering engine for Minecraft mods.', authors: ['Jozufozu'], downloads: 98000000, updated: '2026-09-12T09:00:00Z', kind: 'mods', fileName: 'flywheel-neoforge-1.21.1-1.0.4.jar' },
  { projectId: 238222, name: 'Just Enough Items (JEI)', summary: 'View items and recipes.', authors: ['mezz'], downloads: 389000000, updated: '2026-10-10T09:00:00Z', kind: 'mods', fileName: 'jei-1.21.1-neoforge-19.21.0.247.jar' },
  { projectId: 520110, name: 'Farmer\'s Delight', summary: 'A gentle expansion to farming and cooking in Minecraft.', authors: ['vectorwing'], downloads: 120000000, updated: '2026-08-20T09:00:00Z', kind: 'mods', fileName: 'FarmersDelight-1.21.1-1.2.9.jar' },
  { projectId: 551736, name: 'Distant Horizons', summary: 'See farther without turning your game into a slide show.', authors: ['James Seibel'], downloads: 24000000, updated: '2026-10-08T09:00:00Z', kind: 'mods', fileName: 'DistantHorizons-2.4.0-b-1.21.1-fabric-neoforge.jar', releaseType: 'beta' },
  { projectId: 404465, name: 'FTB Quests', summary: 'Questing mod that lets pack authors write a quest book.', authors: ['FTB'], downloads: 87000000, updated: '2026-07-01T09:00:00Z', kind: 'mods', fileName: null },
  { projectId: 855414, name: "Iron's Spells 'n Spellbooks", summary: 'Upgrade your magic with 80+ spells, scrolls and spellbooks.', authors: ['iron431'], downloads: 61000000, updated: '2026-10-01T09:00:00Z', kind: 'mods', fileName: 'irons_spellbooks-1.21.1-3.5.0.jar' },
  { projectId: 453763, name: 'Fresh Animations', summary: 'Breathing new life into the mobs of Minecraft.', authors: ['FreshLX'], downloads: 18000000, updated: '2026-09-01T09:00:00Z', kind: 'resourcepacks', fileName: 'FreshAnimations_v1.9.4.zip' },
  { projectId: 1015157, name: 'Paxi (NeoForge) [1.20.4 - 1.21.1 ONLY]', summary: 'Load data packs and resource packs globally, in every world.', authors: ['YUNGNICKYOUNG'], downloads: 9800000, updated: '2026-06-02T09:00:00Z', kind: 'mods', fileName: 'Paxi-1.21.1-NeoForge-5.1.3.jar', requires: [1015100] },
  { projectId: 1015100, name: "YUNG's API (NeoForge) [1.20.4 - 1.21.1 ONLY]", summary: "Common code for YUNG's mods.", authors: ['YUNGNICKYOUNG'], downloads: 41000000, updated: '2026-06-02T09:00:00Z', kind: 'mods', fileName: 'YungsApi-1.21.1-NeoForge-5.1.9.jar' },
  { projectId: 354339, name: 'Open Loader', summary: 'Load data and resource packs from a global folder.', authors: ['Darkhax'], downloads: 21000000, updated: '2026-05-11T09:00:00Z', kind: 'mods', fileName: 'OpenLoader-neoforge-1.21.1-21.1.3.jar' },
  { projectId: 627557, name: 'Complementary Shaders - Reimagined', summary: 'Complementary with the classic looks.', authors: ['EminGT'], downloads: 32000000, updated: '2026-09-21T09:00:00Z', kind: 'shaderpacks', fileName: 'ComplementaryReimagined_r5.6.zip' },
]
const colors = ['#7a4b2a', '#5b6b8a', '#3f6e4e', '#6a3f8a', '#8a3f3f', '#4a6a2a', '#2a5a6a', '#9a6a2a', '#2a4a7a']
const projectLogo = (p: MockProject) => logo(p.name[0], colors[p.projectId % colors.length])

// The mock keeps CurseForge's numeric IDs inside and speaks the app's cf: and mr: keys at its edge.
const fileOf = (request: { file?: string; project?: string }) =>
  request.file ? Number(request.file.replace(/^cf:/, '')) : Number(String(request.project).replace(/^(cf|mr):/, '')) * 10
const toFile = (f: { fileId: number; fileName: string | null; releaseType: string; fileDate: string | null }, onModrinth: boolean | null = null) => ({
  file: `cf:${f.fileId}`,
  fileName: f.fileName,
  releaseType: f.releaseType,
  fileDate: f.fileDate,
  onModrinth,
})
// In the dirty scenario, Modrinth hasn't caught up with the newest file of Iron's Spells, only the one before it.
const behindOnModrinth = new Set(scenario === 'dirty' ? ['mods/irons_spellbooks-1.21.1-3.4.0.jar'] : [])

function searchMods(request: { platform: Platform; text: string; kind: string; category: string | null; sort: string; index: number }) {
  const text = request.text.trim().toLowerCase()
  const sorted = catalog
    .filter((p) => p.fileName && p.kind === request.kind && (!text || p.name.toLowerCase().includes(text) || p.summary.toLowerCase().includes(text)))
    .sort((a, b) => (request.sort === 'updated' ? b.updated.localeCompare(a.updated) : b.downloads - a.downloads))
  const modrinth = request.platform === 'modrinth'
  const results = sorted.slice(request.index, request.index + 8).map((p) => ({
    key: `${modrinth ? 'mr' : 'cf'}:${p.projectId}`,
    name: p.name,
    summary: p.summary,
    authors: p.authors,
    downloads: p.downloads,
    updated: p.updated,
    logo: projectLogo(p),
    url: modrinth ? `https://modrinth.com/project/${p.name.toLowerCase().replace(/[^a-z0-9]+/g, '-')}` : cfUrl(p.name.toLowerCase().replace(/[^a-z0-9]+/g, '-')),
    // Modrinth's search doesn't name a file, so packraven picks one when you install.
    file: modrinth ? null : toFile({ fileId: p.projectId * 10, fileName: p.fileName, releaseType: p.releaseType ?? 'release', fileDate: null }),
  }))
  const nextIndex = request.index + 8 < sorted.length ? request.index + 8 : null
  return new Promise((resolve) => setTimeout(() => resolve({ results, nextIndex }), 250))
}

// Mirrors mod_updates::plan's dependency walk over the mock catalog.
function installPlan(fileIds: number[]) {
  const present = new Set(installedMods.map((m) => m.projectId))
  const wanted = fileIds.map((id) => catalog.find((p) => p.projectId * 10 === id)!)
  wanted.forEach((p) => present.add(p.projectId))
  const dependencies: MockProject[] = []
  for (let i = 0; i < wanted.length + dependencies.length; i++) {
    const project = [...wanted, ...dependencies][i]
    for (const id of project.requires ?? []) {
      if (present.has(id)) continue
      present.add(id)
      dependencies.push(catalog.find((p) => p.projectId === id)!)
    }
  }
  return { wanted, dependencies }
}

const names = (projects: MockProject[]) => projects.map((p) => p.name).join(', ').replace(/, ([^,]*)$/, ' and $1')

function changeMods(requests: { fileId: number; replaces: string | null }[]): number {
  // Installs and updates both name files by number inside the mock.
  const installs = installPlan(requests.filter((r) => !r.replaces).map((r) => r.fileId))
  const added = [...installs.wanted, ...installs.dependencies]
  const message = [
    installs.wanted.length ? `Installed ${names(installs.wanted)}.` : '',
    installs.dependencies.length ? `Added ${names(installs.dependencies)} as required.` : '',
  ]
  return updateMods(
    requests.filter((r) => r.replaces).map((r) => ({ path: r.replaces!, fileId: r.fileId })),
    added.flatMap((p) => [`Downloading ${p.kind}/${p.fileName}`, `Placed ${p.kind}/${p.fileName}`]),
    message,
    () => {
      for (const p of added) {
        installedMods.push({ path: `${p.kind}/${p.fileName}`, projectId: p.projectId, fileId: p.projectId * 10, name: p.name, url: null, logo: projectLogo(p), releaseType: p.releaseType ?? 'release', requires: p.requires ?? [], newer: [] })
      }
      installedMods.sort((a, b) => a.name.localeCompare(b.name))
    },
  )
}

function updateMods(requests: { path: string; fileId: number }[], extraLines: string[] = [], extraMessage: string[] = [], extraChange = () => {}): number {
  const files = requests.map((r) => {
    const mod = installedMods.find((m) => m.path === r.path)!
    const file = mod.newer.find((f) => f.fileId === r.fileId) ?? { fileId: r.fileId, fileName: `${mod.name.replace(/\W+/g, '')}-${r.fileId}.jar` }
    return { mod, file }
  })
  return queueModChange(
    [...files.flatMap(({ mod, file }) => [`Downloading ${mod.path.slice(0, mod.path.indexOf('/'))}/${file.fileName}`, `Removed ${mod.path}`, `Placed ${file.fileName}`]), ...extraLines],
    [files.length ? (files.length <= 4 ? `Updated ${names(files.map(({ mod }) => ({ name: mod.name }) as MockProject))}.` : `Updated ${files[0].mod.name}, ${files[1].mod.name} and ${files.length - 2} more.`) : '', ...extraMessage].filter(Boolean).join(' '),
    () => {
      extraChange()
      for (const { mod, file } of files) {
        const folder = mod.path.slice(0, mod.path.indexOf('/'))
        mod.path = `${folder}/${file.fileName}${mod.path.endsWith('.disabled') ? '.disabled' : ''}`
        mod.fileId = file.fileId
        mod.newer = mod.newer.filter((f) => f.fileId > file.fileId)
      }
    },
  )
}

const preflight = {
  dirty: [
    "mods/raven_tweaks-1.2.jar isn't on CurseForge or Modrinth. In Changes, add it to Git or move it to the trash.",
    "The mods folder has two versions of one mod: iris-neoforge-1.8.0.jar and iris-neoforge-1.8.1.jar. In Mods, remove the one you don't want.",
  ],
  ready: [],
  empty: ['Set a CurseForge or Modrinth project ID in Pack settings.'],
  nopack: [],
  newerlock: [],
}[scenario]

type MockEntry = { name: string; path: string; isDir: boolean; size: number; ignored: boolean }

function addEntry(path: string, isDir: boolean) {
  const index = path.lastIndexOf('/')
  const dir = index < 0 ? '' : path.slice(0, index)
  const list = (fx.dirs[dir] ??= listDir(dir)) as MockEntry[]
  if (list.some((e) => e.path === path)) throw `${path} already exists`
  list.push({ name: path.slice(index + 1), path, isDir, size: 0, ignored: false })
  list.sort((a, b) => Number(b.isDir) - Number(a.isDir) || a.name.localeCompare(b.name))
  if (isDir) fx.dirs[path] ??= []
}

function removeEntries(paths: string[]) {
  for (const dir of Object.keys(fx.dirs)) fx.dirs[dir] = (fx.dirs[dir] as MockEntry[]).filter((e) => !paths.includes(e.path))
}

function moveEntry(from: string, to: string) {
  const entry = Object.values(fx.dirs)
    .flat()
    .find((e) => (e as MockEntry).path === from) as MockEntry | undefined
  removeEntries([from])
  addEntry(to, entry?.isDir ?? false)
  for (const dir of Object.keys(fx.dirs)) {
    if (dir !== from && !dir.startsWith(`${from}/`)) continue
    fx.dirs[to + dir.slice(from.length)] = (fx.dirs[dir] as MockEntry[]).map((e) => ({ ...e, path: to + e.path.slice(from.length) }))
    if (dir !== to) delete fx.dirs[dir]
  }
}

// A pretend mod jar, so you can try archive browsing, the image viewer and decompiling.
const jar: Record<string, [string, boolean][]> = {
  '': [['assets', true], ['com', true], ['data', true], ['META-INF', true], ['fabric.mod.json', false], ['README.md', false]],
  assets: [['example', true]],
  'assets/example': [['lang', true], ['textures', true]],
  data: [['example', true]],
  'data/example': [['recipe', true]],
  'data/example/recipe': [['fire_charge.json', false], ['stone_bricks.json', false]],
  'assets/example/lang': [['en_us.json', false]],
  'assets/example/textures': [['block', true]],
  'assets/example/textures/block': [['fire.png', false], ['fire.png.mcmeta', false], ['stone.png', false]],
  com: [['example', true]],
  'com/example': [['ExampleMod$Inner.class', false], ['ExampleMod.class', false]],
  'META-INF': [['MANIFEST.MF', false]],
}

function splitArchive(path: string) {
  const parts = path.split('/')
  const end = parts.findIndex((p) => /\.(jar|zip)$/i.test(p))
  return end < 0 ? null : { archive: parts.slice(0, end + 1).join('/'), inner: parts.slice(end + 1).join('/') }
}

function listDir(path: string) {
  const inside = splitArchive(path)
  if (inside) {
    return (jar[inside.inner] ?? []).map(([name, isDir]) => ({ name, path: `${path}/${name}`, isDir, size: 1024, ignored: false, archive: false, archived: true }))
  }
  const fallback: Record<string, MockEntry[]> = {
    mods: [{ name: 'example-mod.jar', path: 'mods/example-mod.jar', isDir: false, size: 40960, ignored: false }],
    logs: [
      { name: '2026-10-13-1.log.gz', path: 'logs/2026-10-13-1.log.gz', isDir: false, size: 812, ignored: true },
      { name: 'latest.log', path: 'logs/latest.log', isDir: false, size: 4096, ignored: true },
    ],
  }
  const listed = [...((fx.dirs[path] as MockEntry[] | undefined) ?? fallback[path] ?? [])]
  if (path === '' && !listed.some((e) => e.name === 'huge-recipes.json')) listed.push({ name: 'huge-recipes.json', path: 'huge-recipes.json', isDir: false, size: 72e6, ignored: false })
  if (path === '' && !listed.some((e) => e.name === 'README.md')) listed.push({ name: 'README.md', path: 'README.md', isDir: false, size: 2048, ignored: false })
  return listed.map((e) => ({ ...e, archive: !e.isDir && /\.(jar|zip)$/i.test(e.name), archived: false }))
}

const packLoaderIds = { paxi: [418881, 515708, 1015157], openloader: [354339], globalpacks: [317134] }

// The fixtures list the pack folders in resourcepacks without their contents, so we count every
// folder there as a pack, and pretend options.txt turns on only Raven's Resources.
const enabledResourcePacks = new Set(["Raven's Resources"])

function packFolder(path: string, canCreate = true) {
  const isPack = (e: MockEntry) => e.isDir && (path === 'resourcepacks' || listDir(e.path).some((f) => f.name === 'pack.mcmeta'))
  const packs = fx.dirs[path] ? listDir(path).filter((e) => isPack(e) || /\.zip$/i.test(e.name)) : []
  const enabled = (name: string) => (path === 'resourcepacks' ? enabledResourcePacks.has(name) : undefined)
  return { path, exists: !!fx.dirs[path], canCreate, packs: packs.map((e) => ({ path: e.path, name: e.name, zipped: !e.isDir, enabled: enabled(e.name) })) }
}

function packLoaders(kind: 'data' | 'resource') {
  const installed = (ids: number[]) => installedMods.some((m) => ids.includes(m.projectId))
  const folders = {
    paxi: kind === 'data' ? ['config/paxi/datapacks'] : ['config/paxi/resourcepacks'],
    openloader: kind === 'data' ? ['config/openloader/data'] : ['config/openloader/resources'],
    globalpacks: kind === 'data' ? ['global_packs/required_data', 'global_packs/optional_data'] : ['global_packs/required_resources'],
  }
  const names = { paxi: 'Paxi', openloader: 'Open Loader', globalpacks: 'Global Packs' }
  return (Object.keys(names) as (keyof typeof names)[]).map((id) => ({ id, name: names[id], installed: installed(packLoaderIds[id]), projectIds: packLoaderIds[id], folders: folders[id].map((f) => packFolder(f)) }))
}

function ensureDir(path: string, created: string[]) {
  const parts = path.split('/')
  for (let end = 1; end <= parts.length; end++) {
    const dir = parts.slice(0, end).join('/')
    const parent = parts.slice(0, end - 1).join('/')
    if (listDir(parent).some((e) => e.path === dir)) continue
    addEntry(dir, true)
    created.push(dir)
  }
}

function copyIntoPack(sources: string[], pack: string, create: boolean) {
  const created: string[] = []
  const exists = (path: string) => listDir(path.slice(0, path.lastIndexOf('/'))).some((e) => e.path === path)
  if (!exists(`${pack}/pack.mcmeta`)) {
    if (!create) throw `${pack} isn't a pack: it has no pack.mcmeta`
    ensureDir(pack, created)
    addEntry(`${pack}/pack.mcmeta`, false)
    const text = JSON.stringify({ pack: { description: pack.split('/').pop(), pack_format: 48 } }, null, 2) + '\n'
    fx.files[`${pack}/pack.mcmeta`] = { text, size: text.length, unreadable: null, crlf: false, bom: false, archived: false, compressed: false, large: false }
  }
  const innerFiles = (inner: string): string[] => (jar[inner] ? jar[inner].flatMap(([name, isDir]) => (isDir ? innerFiles(`${inner}/${name}`) : [`${inner}/${name}`])) : [inner])
  let existing = 0
  const paths: string[] = []
  for (const source of sources) {
    const inside = source.slice(source.lastIndexOf('.jar/') + 5)
    paths.push(`${pack}/${inside}`)
    for (const file of innerFiles(inside)) {
      const target = `${pack}/${file}`
      if (exists(target)) {
        existing++
        continue
      }
      ensureDir(target.slice(0, target.lastIndexOf('/')), created)
      addEntry(target, false)
      if (!created.includes(pack)) created.push(target)
      fx.files[target] = { ...(readFile(source.slice(0, source.lastIndexOf('.jar/') + 5) + file) as object), archived: false }
    }
  }
  return new Promise((resolve) => setTimeout(() => resolve({ paths, created, existing }), 200))
}

// Exercises headings, tables, task lists, raw HTML, relative images and every kind of link.
const sampleReadme = [
  '<p align="center"><img src="mods/example-mod.jar/assets/example/textures/block/stone.png" width="64" alt="Pack icon"></p>',
  '',
  "# Raven's High Fantasy",
  '',
  'A **fantasy** pack with _dragons_, dungeons and `kubejs` tweaks. See [Installing](#installing) or the [config notes](config/apotheosis/apotheosis.cfg).',
  '',
  '![Fire](mods/example-mod.jar/assets/example/textures/block/fire.png) ![Badge](https://img.shields.io/badge/minecraft-1.21.1-green)',
  '',
  '## Installing',
  '',
  '1. Install the CurseForge app',
  '2. Import the pack',
  '   - Give it **8 GB** of memory',
  '',
  '## Roadmap',
  '',
  '- [x] Dragons',
  '- [ ] More dungeons',
  '',
  '| Mod | Version |',
  '| --- | --- |',
  '| Apotheosis | 8.1.2 |',
  '| Iron\'s Spells | 3.4.0 |',
  '',
  '> Report bugs on [GitHub](https://github.com/muon-rw/Ravens-High-Fantasy/issues).',
  '',
  '<script>alert("no")</script>',
  '',
  '```js',
  "ServerEvents.recipes(event => event.remove({ id: 'minecraft:stick' }))",
  '```',
  '',
  ...Array.from({ length: 40 }, (_, i) => `Filler paragraph ${i + 1} so the preview scrolls.\n`),
].join('\n')

function readFile(path: string) {
  const archived = !!splitArchive(path)?.inner
  if (path.endsWith('README.md')) return { text: sampleReadme, size: sampleReadme.length, unreadable: null, crlf: false, bom: false, archived, compressed: false, large: false }
  if (path.endsWith('.gz')) {
    const log = '[12:00:00] [main/INFO] [net.minecraft.server.Main/]: Starting server\n[12:00:01] [main/WARN] [mixin/]: Reference map not found\n'
    return { text: log, size: 120, unreadable: null, crlf: false, bom: false, archived: false, compressed: true }
  }
  const binary = { text: null, size: 2048, unreadable: "This is a binary file, so packraven can't edit it.", crlf: false, bom: false, archived }
  if (path.endsWith('fire.png.mcmeta')) return { text: '{ "animation": { "frametime": 4, "interpolate": true } }', size: 60, unreadable: null, crlf: false, bom: false, archived }
  if (path.endsWith('.mcmeta') && archived) throw `${path} isn't in the archive`
  if (/\/recipe\/[^/]+\.json$/.test(path)) {
    const recipe = { type: 'minecraft:crafting_shapeless', ingredients: ['minecraft:blaze_powder', 'minecraft:coal', 'minecraft:gunpowder'], result: { id: 'minecraft:fire_charge', count: 3 } }
    const text = JSON.stringify(recipe, null, 2) + '\n'
    return (fx.files[path] as object | undefined) ?? { text, size: text.length, unreadable: null, crlf: false, bom: false, archived, compressed: false, large: false }
  }
  if (/\.(png|jar|zip|nbt|dat|class|ogg|mp4)$/.test(path)) return binary
  return (fx.files[path] as object | undefined) ?? { text: `// ${path}\n`, size: 0, unreadable: null, crlf: false, bom: false, archived }
}

// Draws a 16 px texture, or for fire.png a strip of four frames, as a PNG data URL.
// A small pixel-art landscape, drawn at 16px and scaled up the way a pack author's icon often is.
function mockServerIcon() {
  const canvas = document.createElement('canvas')
  canvas.width = canvas.height = 64
  const context = canvas.getContext('2d')!
  context.imageSmoothingEnabled = false
  for (let y = 0; y < 16; y++) {
    for (let x = 0; x < 16; x++) {
      const hill = 10 - Math.round(2 * Math.sin(x / 2.5))
      context.fillStyle = y < hill ? (y < 4 ? '#20291b' : '#463b1c') : y === hill ? '#7f9b5c' : (x + y) % 3 ? '#4d6b2f' : '#3c5524'
      if (y >= 3 && y <= 5 && x >= 10 && x <= 12) context.fillStyle = '#e3c46f'
      context.fillRect(x * 4, y * 4, 4, 4)
    }
  }
  return canvas.toDataURL('image/png')
}

let serverIcon: string | null = scenario === 'empty' ? null : 'pending'
// A fresh setup hasn't accepted the Minecraft EULA or signed in to Minecraft yet.
let eulaAccepted = scenario !== 'empty'
const javaByMajor: Record<string, string> = {}
let javaDownloadCancelled = false
// A machine with CurseForge's Java 17 and 21, a few JDKs of its own, and no Java 25.
const mockJavas = [
  { path: 'C:/curseforge/minecraft/Install/java/java-runtime-delta/bin/java.exe', major: 21, version: '21.0.8', distribution: 'Temurin', source: 'curseForge', foreignArch: null },
  { path: 'C:/Program Files/Java/graalvm-jdk-21.0.7+8.1/bin/java.exe', major: 21, version: '21.0.7', distribution: 'GraalVM', source: 'path', foreignArch: null },
  { path: 'C:/Program Files/Eclipse Adoptium/jdk-21.0.1.12-hotspot/bin/java.exe', major: 21, version: '21.0.1', distribution: 'Temurin', source: 'system', foreignArch: null },
  { path: 'C:/Users/you/.jdks/corretto-19.0.2/bin/java.exe', major: 19, version: '19.0.2', distribution: 'Corretto', source: 'intelliJ', foreignArch: null },
  { path: 'C:/curseforge/minecraft/Install/java/java-runtime-gamma/bin/java.exe', major: 17, version: '17.0.8', distribution: 'Temurin', source: 'curseForge', foreignArch: null },
  { path: 'C:/Program Files/Eclipse Adoptium/jdk-17.0.16.8-hotspot/bin/java.exe', major: 17, version: '17.0.16', distribution: 'Temurin', source: 'javaHome', foreignArch: null },
  { path: 'C:/Users/you/.gradle/jdks/eclipse_adoptium-17-amd64-windows/jdk-17.0.16+8/bin/java.exe', major: 17, version: '17.0.16', distribution: 'Temurin', source: 'gradle', foreignArch: null },
]
let minecraftAccount: { name: string; uuid: string } | null = scenario === 'empty' ? null : { name: 'muonr', uuid: '0f6c1e5a2b3d4c5e8f9a0b1c2d3e4f50' }
const SIGN_IN_FIRST = 'Sign in to Minecraft to launch the client.'
let autoUpdate = true
// The dirty scenario has an update waiting, so screenshots cover the rail's restart button.
const mockPackravenReleases = [
  {
    version: '0.2.0',
    publishedAt: '2026-10-28',
    body: "### Added\n- Sign in to Minecraft right from the Run view\n\n### Changed\n- Test servers now use the `online-mode` from your pack's server.properties",
  },
  {
    version: '0.1.9',
    publishedAt: '2026-10-27',
    body: '### Fixed\n- Reduced number of permission prompt popups on macOS',
  },
  {
    version: '0.1.0',
    publishedAt: '2026-10-19',
    body: '### Added\n- First public build of packraven',
  },
]
let updatedFrom: string | null = new URLSearchParams(location.search).get('updated')
let currentVersion = updatedFrom ? '0.2.0' : '0.1.0'
let updatePhase: Record<string, unknown> = scenario === 'dirty' && !updatedFrom ? { phase: 'ready', version: '0.2.0' } : { phase: 'upToDate' }
const updateView = () => ({ current: currentVersion, auto: autoUpdate, supported: true, blocked: null, updatedFrom, status: updatePhase })
let cancelSignIn = () => {}

function mockImage(path: string, older = false) {
  const canvas = document.createElement('canvas')
  const frames = path.includes('fire') ? 4 : 1
  // The older version of a changed texture has a moss corner, so the image diff has a few changed pixels to show.
  canvas.width = 16
  canvas.height = 16 * frames
  const context = canvas.getContext('2d')!
  const colors = ['#d9480f', '#f08c00', '#fcc419', '#e8590c']
  for (let f = 0; f < frames; f++) {
    for (let y = 0; y < 16; y++) {
      for (let x = 0; x < 16; x++) {
        if (frames === 1) context.fillStyle = older && x < 6 && y < 6 ? ((x + y) % 3 ? '#4d6b2f' : '#7f9b5c') : (x + y) % 4 < 2 ? '#868e96' : '#495057'
        else context.fillStyle = (x * 7 + y * 3 + f * 5) % 6 < 3 ? colors[f] : 'transparent'
        context.fillRect(x, f * 16 + y, 1, 1)
      }
    }
  }
  return canvas.toDataURL('image/png')
}

function dataUrlBytes(url: string) {
  return Uint8Array.from(atob(url.split(',')[1]), (c) => c.charCodeAt(0)).buffer
}

// A short NeoForge log that grows by a line on each read, so you can watch the viewer follow it.
const sampleLog = [
  '[12:00:00] [main/INFO] [cpw.mods.modlauncher.Launcher/MODLAUNCHER]: ModLauncher running',
  '[12:00:01] [main/DEBUG] [mixin/]: Registering new injector for @SugarWrapper',
  '[12:00:02] [main/WARN] [mixin/]: Reference map \'example.refmap.json\' for example.mixins.json could not be read',
  '[12:00:03] [Render thread/ERROR] [net.minecraft.client.Minecraft/]: Failed to load texture',
  'java.io.FileNotFoundException: example:textures/block/missing.png',
  '\tat net.minecraft.server.packs.resources.ResourceManager.getResourceOrThrow(ResourceManager.java:25)',
  '\tat net.minecraft.client.renderer.texture.SimpleTexture.load(SimpleTexture.java:40)',
  '[12:00:04] [main/INFO] [net.neoforged.fml.loading.moddiscovery.ModDiscoverer/SCAN]: Mod List:',
  '\t\tName Version (Mod Id)',
  '\t\t[NoCube\'s] Realm RPG: Treasure Balloons 1.0.0 (realmrpg_balloons)',
  '[12:00:05] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: Done (3.2s)!',
]
let logReads = 0

const sampleNbt = {
  compression: 'gzip',
  rootName: '',
  root: {
    type: 'compound',
    value: [
      [
        'servers',
        {
          type: 'list',
          value: {
            itemType: 'compound',
            items: [
              {
                type: 'compound',
                value: [
                  ['name', { type: 'string', value: 'Raven Test Server' }],
                  ['ip', { type: 'string', value: 'localhost:25565' }],
                  ['acceptTextures', { type: 'byte', value: 1 }],
                ],
              },
            ],
          },
        },
      ],
      ['DataVersion', { type: 'int', value: 3955 }],
      ['Seed', { type: 'long', value: '-1234567890123456789' }],
      ['Heights', { type: 'longArray', value: { length: 600, items: ['0', '1', '2'] } }],
    ],
  },
}

const callbacks = new Map<number, (message: unknown) => void>()
const listeners: { event: string; handler: number }[] = []

// Lets you fake backend events from the console, such as a file drag from Explorer:
// __mockEmit('tauri://drag-over', { position: { x: 300, y: 200 } })
function emit(event: string, payload: unknown) {
  for (const l of listeners.filter((l) => l.event === event)) callbacks.get(l.handler)?.({ event, id: l.handler, payload })
}

// The mock's server and client stand in for processes: they run until you stop or kill them.
let serverRunning = false
let clientRunning = false

function startMockServer() {
  serverRunning = true
  emit('test-event', { kind: 'server-started', detail: '' })
  setTimeout(() => {
    emit('job-log', { job: 'server', stream: 'info', text: 'Done (4.213s)! For help, type "help"' })
    emit('test-event', { kind: 'server-ready', detail: '' })
  }, 400)
}

function stopMockServer() {
  serverRunning = false
  emit('test-event', { kind: 'server-stopped', detail: '' })
}

function startMockClient() {
  clientRunning = true
  emit('test-event', { kind: 'client-started', detail: '' })
}

const noRunArgs = () => ({ clientJvmArgs: [] as string[], clientArgs: [] as string[], serverJvmArgs: [] as string[], serverArgs: [] as string[] })
let runArgs = noRunArgs()
let appRunArgs = noRunArgs()

type MockTextQuery = { pattern: string; regex: boolean; matchCase: boolean; wholeWord: boolean; include: string; exclude: string; includeIgnored: boolean; replace: string | null }
let lastReplace: { label: string; before: Record<string, string> } | null = null

function textRegex(q: MockTextQuery) {
  let body = q.regex ? q.pattern : q.pattern.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
  if (q.wholeWord) body = `\\b(?:${body})\\b`
  try {
    return new RegExp(body, q.matchCase ? 'gu' : 'giu')
  } catch (e) {
    throw `That isn't a valid regex: ${(e as Error).message.replace(/^.*: /, '').toLowerCase()}.`
  }
}

function mockGlob(list: string) {
  const patterns = list.split(',').map((p) => p.trim().replace(/^\.?\//, '').replace(/\/$/, '')).filter(Boolean)
  const toRegex = (p: string) => new RegExp(`(^|/)${p.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*\*/g, '\0').replace(/\*/g, '[^/]*').replace(/\0/g, '.*')}(/|$)`, 'i')
  return patterns.length ? patterns.map(toRegex) : null
}

function textFiles(q: MockTextQuery) {
  const include = mockGlob(q.include)
  const exclude = mockGlob(q.exclude)
  const entries = Object.values(fx.dirs).flat() as MockEntry[]
  const ignored = (path: string) => entries.some((e) => e.ignored && (path === e.path || path.startsWith(`${e.path}/`)))
  return [...new Set([...entries.filter((e) => !e.isDir).map((e) => e.path), ...Object.keys(fx.files)])]
    .filter((path) => (q.includeIgnored || !ignored(path)) && (!include || include.some((r) => r.test(path))) && !exclude?.some((r) => r.test(path)))
    .map((path) => ({ path, text: (readFile(path) as { text: string | null }).text }))
    .filter((file): file is { path: string; text: string } => file.text !== null)
    .sort((a, b) => a.path.toLowerCase().localeCompare(b.path.toLowerCase()))
}

function textMatches(text: string, q: MockTextQuery) {
  const regex = textRegex(q)
  return text.split('\n').flatMap((line, index) =>
    [...line.matchAll(regex)]
      .filter((m) => m[0])
      .map((m) => {
        const indent = line.length - line.trimStart().length
        const from = m.index! - Math.min(indent, m.index!) > 30 ? m.index! - 30 : Math.min(indent, m.index!)
        const prefix = from > Math.min(indent, m.index!) ? '…' : ''
        return {
          line: index + 1,
          column: m.index!,
          length: m[0].length,
          preview: prefix + line.slice(from, from + 250) + (from + 250 < line.length ? '…' : ''),
          start: prefix.length + m.index! - from,
          end: prefix.length + m.index! - from + m[0].length,
          replacement: q.replace === null ? null : m[0].replace(new RegExp(regex.source, regex.flags.replace('g', '')), q.regex ? q.replace : q.replace.replace(/\$/g, '$$$$')),
        }
      }),
  )
}

const handlers: Record<string, (args: Record<string, unknown>) => unknown> = {
  search_text: (a) => {
    const q = a.query as MockTextQuery
    if (!q.pattern) return { files: [], total: 0, truncated: false, searched: 0, tooLarge: 0 }
    const files = textFiles(q)
    const found = files.map((f) => ({ path: f.path, matches: textMatches(f.text, q) })).filter((f) => f.matches.length)
    return { files: found, total: found.reduce((n, f) => n + f.matches.length, 0), truncated: false, searched: files.length, tooLarge: 0 }
  },
  replace_text: (a) => {
    const q = a.query as MockTextQuery
    const before: Record<string, string> = {}
    let replaced = 0
    let missed = 0
    for (const target of a.targets as { path: string; matches: [number, number][] }[]) {
      const text = (readFile(target.path) as { text: string | null }).text ?? ''
      const wanted = new Set(target.matches.map(([line, column]) => `${line}:${column}`))
      const lines = text.split('\n')
      let hits = 0
      const next = lines.map((line, index) => {
        let out = ''
        let copied = 0
        for (const m of textMatches(line, q)) {
          if (!wanted.has(`${index + 1}:${m.column}`)) continue
          out += line.slice(copied, m.column) + m.replacement
          copied = m.column + m.length
          hits++
        }
        return out + line.slice(copied)
      })
      missed += wanted.size - hits
      if (!hits) continue
      before[target.path] = text
      fx.files[target.path] = { ...(readFile(target.path) as object), text: next.join('\n') }
      replaced += hits
    }
    const files = Object.keys(before)
    const label = `Replace in ${files.length === 1 ? files[0].slice(files[0].lastIndexOf('/') + 1) : `${files.length} items`}`
    if (files.length) lastReplace = { label, before }
    return { label, files, replaced, missed, undoable: true }
  },
  list_packs: () => (scenario === 'nopack' ? { active: null, packs: [], instancesRoot: 'C:/Instances' } : { active: activePack, packs: [pack, otherPack], instancesRoot: 'C:/Instances' }),
  switch_pack: (a) => (activePack = String(a.dir)),
  resync_mod_list: () => null,
  overview: () => {
    if (scenario === 'newerlock') throw "A newer version of packraven wrote this pack's mods.lock.json. Update packraven in App settings, then try again."
    return {
      name: activePack === otherPack.dir ? otherPack.name : pack.name,
      version: bcc.version,
      bccInstalled: bcc.installed,
      minecraft: fx.minecraft,
      loader: fx.loader,
      lockCount: fx.lockCount,
      addons: scenario === 'empty' ? { mods: 0, resourcePacks: 0, shaderPacks: 0, dataPacks: 0, other: 0 } : { mods: fx.lockCount - 14, resourcePacks: 9, shaderPacks: 2, dataPacks: 3, other: 0 },
      unhosted: scenario === 'dirty' ? ['mods/raven_tweaks-1.2.jar'] : [],
      notOnCurseforge: notOn('curseforge').map((path) => candidateView('curseforge', path)),
      notOnModrinth: notOn('modrinth').map((path) => candidateView('modrinth', path)),
      modrinthError: null,
      releasesMissingModrinth: releases.filter((r) => r.modrinthMissing).length,
      releasesMissingServerPack: releases.filter((r) => r.serverPackMissing).length,
      modListError: null,
      watchError: null,
      branch: 'master',
      ahead: scenario === 'dirty' ? 1 : 0,
      behind: 0,
      incoming: null,
      changed: fx.changed,
      lastVersion: scenario === 'empty' ? null : '0.1.0',
      lastTag: scenario === 'empty' ? null : 'v0.1.0',
      devJars: scenario === 'dirty' ? ['raven_core-dev.jar'] : [],
      projectId: config.curseforge.project_id,
      modrinthProjectId: config.modrinth.project_id,
      runningJobs: [],
      serverRunning,
      clientRunning,
      hooks: 'current',
    }
  },
  start_test: ({ connect }) => {
    if (connect && !minecraftAccount) throw SIGN_IN_FIRST
    if (connect) emit('test-event', { kind: 'player-resolved', detail: minecraftAccount!.name })
    return mockJob('test', ['Syncing changed files to the test server folder'], 'Server starting', () => {
      emit('test-event', { kind: 'server-online', detail: 'true' })
      startMockServer()
      if (connect) startMockClient()
    })
  },
  start_client: () => {
    if (!minecraftAccount) throw SIGN_IN_FIRST
    emit('test-event', { kind: 'player-resolved', detail: minecraftAccount.name })
    return mockJob('client', ['Launching the client'], '', startMockClient)
  },
  server_command: ({ line }) => {
    emit('job-log', { job: 'server', stream: 'info', text: `> ${line}` })
    if (line === 'stop') setTimeout(stopMockServer, 300)
  },
  kill_process: ({ which }) => {
    if (which === 'server') stopMockServer()
    else {
      clientRunning = false
      emit('test-event', { kind: 'client-stopped', detail: '' })
    }
  },
  accept_minecraft_eula: () => {
    eulaAccepted = true
  },
  minecraft_account: () => minecraftAccount,
  minecraft_sign_in: async () => {
    const cancelled = await new Promise<boolean>((resolve) => {
      cancelSignIn = () => resolve(true)
      setTimeout(() => resolve(false), 1500)
    })
    if (cancelled) return null
    minecraftAccount = { name: 'muonr', uuid: '0f6c1e5a2b3d4c5e8f9a0b1c2d3e4f50' }
    return minecraftAccount
  },
  minecraft_sign_in_cancel: () => cancelSignIn(),
  minecraft_sign_out: () => {
    minecraftAccount = null
  },
  get_settings: () => ({ instanceDir: pack.dir, packs: [pack.dir], installDir: 'C:/curseforge/minecraft/Install', javaByMajor: { ...javaByMajor }, defenderExcluded: [], defenderDismissed: false, minecraftEulaAccepted: eulaAccepted, editorAskMb: 50, editorMaxMb: 100, gitName: '', gitEmail: '', autoUpdate, runArgs: appRunArgs }),
  save_settings: (a) => (appRunArgs = (a.settings as { runArgs: typeof appRunArgs }).runArgs),
  update_status: () => updateView(),
  check_for_update: () => updateView(),
  install_update: () => {
    // We play a short download so you can see the progress bar, then act as if the restart finished.
    for (let percent = 0; percent <= 100; percent += 10) {
      setTimeout(() => {
        if (percent < 100) {
          updatePhase = { phase: 'downloading', version: '0.2.0', percent }
        } else {
          updatedFrom = currentVersion
          currentVersion = '0.2.0'
          updatePhase = { phase: 'upToDate' }
        }
        emit('self-update-changed', updateView())
      }, percent * 30)
    }
  },
  set_auto_update: ({ enabled }) => {
    autoUpdate = enabled as boolean
    emit('self-update-changed', updateView())
  },
  update_changelog: ({ since, through }) => {
    const s = String(since)
    const t = String(through)
    return mockPackravenReleases.filter((r) => (s && s < t ? r.version > s && r.version <= t : r.version === t))
  },
  dismiss_update_changelog: () => {
    updatedFrom = null
  },
  github_identity: () => ({ name: 'Muon', email: '1234567+muon-rw@users.noreply.github.com' }),
  git_pull_preview: () => ({
    incoming: { added: 1, updated: 3, removed: 0 },
    conflicts: [{ name: 'sodium-neoforge-0.6.13.jar', mine: 'sodium-neoforge-0.6.14.jar', theirs: 'sodium-neoforge-0.6.15.jar' }],
  }),
  get_pack_config: () => config,
  // The real backend reclassifies every mod; the mock only settles the one you changed.
  set_side_override: (a) => {
    const mod = serverSides.find((m) => m.key === a.key)
    if (!mod || a.mode === 'auto') return
    Object.assign(mod, a.mode === 'keep' ? { side: 'both', reason: 'Kept on the server', override: 'keep' } : { side: 'client', reason: 'Marked client-only', override: 'client' })
  },
  save_pack_config: (a) => Object.assign(config, a.config),
  get_server_icon: () => (serverIcon === 'pending' ? (serverIcon = mockServerIcon()) : serverIcon),
  set_server_icon: () => (serverIcon = mockServerIcon()),
  use_project_logo_as_server_icon: () => (serverIcon = mockServerIcon()),
  remove_server_icon: () => (serverIcon = null),
  server_launch: () => {
    const args = `@libraries/net/neoforged/neoforge/${fx.loader.replace(/^neoforge-/, '')}`
    return { windows: `${args}/win_args.txt`, unix: `${args}/unix_args.txt` }
  },
  get_webhook: () => 'https://discord.com/api/webhooks/0000/example',
  git_version: () => '2.56.0.windows.2',
  credentials_status: () => ({ builtInApiKey: true, upload: { source: { kind: 'env', name: 'CURSEFORGE_TOKEN' }, present: true }, modrinth: { source: { kind: 'stored' }, present: scenario !== 'empty' }, github: { source: { kind: 'stored' }, present: true }, keychain: 'Windows Credential Manager' }),
  env_var_names: () => ['APPDATA', 'CF_DOWNLOAD_TOKEN', 'CURSEFORGE_TOKEN', 'PATH'],
  defender_status: () => ({ active: true, exclusionsReadable: false, missing: [], prompt: false }),
  git_overview: () => ({ status: gitStatus(), commits: fx.commits }),
  git_status: () => gitStatus(),
  git_diff: (a) => diffFor(String(a.path), Boolean(a.staged)),
  git_blob: (a) => dataUrlBytes(mockImage(String(a.path), a.version === 'head' || String(a.path).includes('feather'))),
  git_apply_lines: (a) => console.info('[mock] applied lines', a.action, a.path, a.selected),
  list_dir: (a) => listDir(String(a.path)),
  read_file: (a) =>
    String(a.path).endsWith('huge-recipes.json')
      ? a.allowLarge
        ? { text: '[\n' + '  {"type": "minecraft:crafting_shaped"},\n'.repeat(20000) + '  {}\n]\n', size: 72e6, unreadable: null, crlf: false, bom: false, archived: false, compressed: false, large: true }
        : { text: null, size: 72e6, unreadable: 'This file is 72.0 MB, over the 50.0 MB you set to ask about. You can open it in a simplified editor without checking or folding.', crlf: false, bom: false, archived: false, compressed: false, large: true }
      : readFile(String(a.path)),
  read_bytes: (a) => dataUrlBytes(mockImage(String(a.path))),
  read_nbt: () => sampleNbt,
  read_log: (a) => {
    if (!a.offset) return { text: `${sampleLog.join('\n')}\n`, offset: 1, reset: true, truncated: false, followable: !String(a.path).endsWith('.gz') }
    logReads++
    return { text: `[12:00:${String(5 + logReads).padStart(2, '0')}] [Server thread/INFO] [net.minecraft.server.MinecraftServer/]: Tick ${logReads}\n`, offset: 1 + logReads, reset: false, truncated: false, followable: true }
  },
  media_path: (a) => `${pack.dir}/${a.path}`,
  pack_loaders: (a) => new Promise((resolve) => setTimeout(() => resolve(packLoaders(a.kind as 'data' | 'resource')), 150)),
  pack_folder: (a) => packFolder(String(a.path).replace(/^\/+|\/+$/g, '')),
  copy_into_pack: (a) => copyIntoPack((a.sources as string[]) ?? [], String(a.pack), Boolean(a.create)),
  decompile_class: (a) =>
    new Promise((resolve) =>
      setTimeout(() => resolve(`package com.example;\n\n// Decompiled from ${a.path}\npublic class ExampleMod {\n    public static final String ID = "example";\n\n    static class Inner {\n    }\n}\n`), 600),
    ),
  extract_archive: (a) => {
    if (String(a.path).endsWith('.gz')) {
      const file = String(a.path).slice(0, -3)
      addEntry(file, false)
      return file
    }
    const folder = String(a.path).replace(/\.(jar|zip)$/i, '')
    addEntry(folder, true)
    return folder
  },
  compress_paths: (a) => {
    const first = (a.paths as string[])[0]
    const zip = `${first.replace(/\.[^./]+$/, '')}.zip`
    addEntry(zip, false)
    return zip
  },
  absolute_path: (a) => `${pack.dir}/${a.path}`,
  create_file: (a) => addEntry(String(a.path), false),
  create_dir: (a) => addEntry(String(a.path), true),
  trash_paths: (a) => removeEntries(a.paths as string[]),
  rename_path: (a) => moveEntry(String(a.from), String(a.to)),
  rename_paths: (a) => {
    const pairs = (a.renames as [string, string][]).map(([from, name]): [string, string] => [from, `${from.includes('/') ? from.slice(0, from.lastIndexOf('/') + 1) : ''}${name}`])
    // Renames can swap names, so we move everything aside before giving out the new names.
    pairs.forEach(([from], i) => moveEntry(from, `${from}.renaming-${i}`))
    pairs.forEach(([from, to], i) => moveEntry(`${from}.renaming-${i}`, to))
    return pairs
  },
  move_paths: (a) =>
    (a.paths as string[]).map((from) => {
      const to = `${a.dir ? `${a.dir}/` : ''}${from.slice(from.lastIndexOf('/') + 1)}`
      moveEntry(from, to)
      return [from, to]
    }),
  git_ignore: () => [],
  git_ignore_options: (a) => {
    const paths = a.paths as string[]
    return [
      { label: paths.length === 1 ? 'Only this file' : `Only these ${paths.length} files`, patterns: paths.map((p) => `/${p}`) },
      { label: 'All .json files anywhere', patterns: ['*.json'] },
    ]
  },
  git_ignore_preview: (a) => ({ untracked: (a.patterns as string[]).filter((p) => p.trim()).map((p) => p.replace(/^\//, '')), tracked: ['config/tracked.json'] }),
  git_untrack_preview: (a) => {
    const paths = a.paths as string[]
    return { ignored: paths.filter((p) => trackedIgnored.includes(p)), uncovered: paths.filter((p) => !trackedIgnored.includes(p)) }
  },
  git_keep_tracking: (a) => {
    trackedIgnored = trackedIgnored.filter((p) => !(a.paths as string[]).includes(p))
    return []
  },
  git_untrack: (a) => {
    trackedIgnored = trackedIgnored.filter((p) => !(a.paths as string[]).includes(p))
  },
  copy_files: () => null,
  copy_paths: (a) => console.info('[mock] copied paths', a),
  open_with_default_app: (a) => console.info('[mock] opened', a.path),
  default_app: (a) => ({ png: 'Photos', zip: 'Windows Explorer' })[String(a.extension)] ?? null,
  index_files: () => ({
    paths: Object.values(fx.dirs)
      .flat()
      .map((e) => ((e as MockEntry).isDir ? `${(e as MockEntry).path}/` : (e as MockEntry).path)),
    truncated: false,
  }),
  import_paths: (a) => ({
    created: (a.sources as string[]).map((source) => {
      const path = `${a.dir ? `${a.dir}/` : ''}${source.split(/[\\/]/).pop()}`
      addEntry(path, false)
      return path
    }),
    moved: [],
  }),
  drag_out: () => null,
  file_history: () => ({ undo: lastReplace?.label ?? 'Rename options.txt', redo: null }),
  file_undo: () => {
    if (!lastReplace) return null
    const { label, before } = lastReplace
    lastReplace = null
    for (const [path, text] of Object.entries(before)) fx.files[path] = { ...(readFile(path) as object), text }
    return { label, moved: [], removed: [], added: [], changed: Object.keys(before) }
  },
  paste_files: () => {
    throw 'The clipboard has no files to paste.'
  },
  releases: () => releases,
  get_notes: () => notes,
  changelog_preview: () => ({ diff: modDiff, markdown: renderChangelog() }),
  publish_preflight: () => {
    const stem = `${pack.name.replace(/[^\w -]/g, '').trim().replace(/ /g, '-')}-{version}`
    const tagTaken = scenario !== 'empty' && bcc.version === '0.1.0'
    const invalid = bcc.version === null ? null : versionProblem(bcc.version)
    const blocker =
      bcc.version === null
        ? 'Install Better Compatibility Checker in Changes. packraven keeps the pack version in its config.'
        : invalid
          ? `${invalid} Change the version to publish.`
          : tagTaken
            ? 'You already released 0.1.0 (tag v0.1.0 exists). Change the version to publish a new release.'
            : null
    const blockedOn = (platform: Platform, name: string) => {
      const blocked = notOn(platform)
      if (!blocked.length) return []
      return blocked.length === 1 ? [`${blocked[0]} isn't on ${name}. Include it in the ${name} pack or leave it out, below.`] : [`${blocked.length} files aren't on ${name}. Include them in the ${name} pack or leave them out, below.`]
    }
    const modrinth = [...blockedOn('curseforge', 'CurseForge'), ...blockedOn('modrinth', 'Modrinth')]
    return {
      problems: [...(blocker ? [blocker] : []), ...preflight, ...modrinth],
      tagTaken,
      warnings: scenario === 'dirty' ? ['betterclouds-neoforge-1.3.2.jar is disabled, so packraven leaves it out of the export.'] : [],
      client: { displayName: `${pack.name} {version}`, fileName: `${stem}.zip` },
      server: { displayName: 'Server Pack {version}', fileName: `${stem}-server.zip` },
      modrinthFile: `${stem}.mrpack`,
      modrinthShipped: decidedFiles('modrinth', 'allowed_override_jars').concat(overrideChoices('modrinth').byAuthor.flatMap((group) => group.files)),
      modrinthLeftOut: decidedFiles('modrinth', 'exclude'),
      curseforgeShipped: decidedFiles('curseforge', 'allowed_override_jars'),
      curseforgeLeftOut: decidedFiles('curseforge', 'exclude'),
    }
  },
  build_modrinth_export: () =>
    mockJob('modrinth-export', ['Modrinth pack: 391 Modrinth files, 4 override files'], `C:/Users/you/AppData/Roaming/dev.muon.packraven/builds/${bcc.version ?? 'dev'}/pack.mrpack`, () => {}),
  upload_release_server_pack: (a) =>
    mockJob('publish', ['Uploading the server pack', 'Uploaded the server pack as file 7300001'], `Uploaded the ${a.version} server pack to CurseForge as file 7300001`, () => {
      const release = releases.find((r) => r.version === a.version)!
      release.serverPackMissing = null
      release.curseforge = { ...release.curseforge!, serverStatus: 'Not public yet' }
    }),
  upload_release_to_modrinth: (a) =>
    mockJob('publish', ['Uploading the pack to Modrinth', 'Uploaded Modrinth version'], `Uploaded ${a.version} to Modrinth`, () => {
      const release = releases.find((r) => r.version === a.version)!
      release.modrinth = modrinthRelease(String(a.version))
      release.modrinthMissing = null
    }),
  set_override_choice: (a) => {
    const paths = a.paths as string[]
    const lists = choicesOf(a.platform as Platform)
    const keep = (list: string[]) => list.filter((entry) => !paths.some((path) => covers(path, entry)))
    lists.allowed_override_jars = [...keep(lists.allowed_override_jars), ...(a.choice === 'ship' ? paths : [])].sort()
    lists.exclude = [...keep(lists.exclude), ...(a.choice === 'leaveOut' ? paths : [])].sort()
  },
  look_up_override_candidates: () => {},
  override_choices: (a) => overrideChoices(a.platform as Platform),
  set_modrinth_author: (a) => {
    const author = String(a.author)
    const others = config.modrinth.allowed_authors.filter((x) => x !== author)
    config.modrinth.allowed_authors = a.allowed ? [...others, author].sort() : others
  },
  // Like the lock entry, the file Modrinth hosts drops it from the files to decide about.
  use_modrinth_build: (a) => {
    if (a.using) usingModrinthBuild.add(String(a.path))
    else usingModrinthBuild.delete(String(a.path))
  },
  modrinth_project_title: (a) => (String(a.id) === 'ravens-high-fantasy' ? "Raven's High Fantasy" : null),
  unfinished_releases: () =>
    [...releases]
      .reverse()
      .filter((r) => r.serverPackMissing || r.modrinthMissing || unfinishedSteps.has(r.version))
      .map((r) => ({
        version: r.version,
        displayName: r.displayName,
        serverPackMissing: r.serverPackMissing,
        modrinthMissing: r.modrinthMissing,
        modrinthError: r.modrinthMissing ? 'Modrinth rejected the upload (502 Bad Gateway)' : null,
        pending: unfinishedSteps.get(r.version)?.pending ?? [],
        packChanged: unfinishedSteps.get(r.version)?.packChanged ?? false,
      })),
  mark_server_pack_uploaded: (a) => {
    const release = releases.find((r) => r.version === a.version)!
    release.serverPackMissing = null
    release.curseforge = { ...release.curseforge!, serverStatus: 'Uploaded by you' }
  },
  finish_release: (a) =>
    mockJob('publish', ['Uploading the server pack', 'Uploaded the server pack as file 7300001', `Committed "Release ${a.version}" and tagged v${a.version}`, 'Pushed the release commit and tag'], `Finished publishing ${a.version}`, () => {
      const release = releases.find((r) => r.version === a.version)!
      if (release.serverPackMissing) release.curseforge = { ...release.curseforge!, serverStatus: 'Not public yet' }
      if (release.modrinthMissing) release.modrinth = modrinthRelease(String(a.version))
      release.serverPackMissing = null
      release.modrinthMissing = null
      unfinishedSteps.delete(String(a.version))
    }),
  published_to: (a) => {
    const release = releases.find((r) => r.version === a.version)
    return release ? { curseforge: !!release.curseforge, modrinth: !!release.modrinth, serverPackMissing: release.serverPackMissing, modrinthError: null } : null
  },
  publish: (a) => {
    const version = (a.options as { version: string }).version
    const fileId = 7200000 + releases.length
    const serverLine = serverPackFails ? "Couldn't upload the server pack" : `Uploaded the server pack as file ${fileId + 1}`
    mockJob(
      'publish',
      ['Uploading the client pack', `Uploaded the client pack as file ${fileId}`, 'Uploading the server pack', serverLine, `Committed "Release ${version}" and tagged v${version}`],
      serverPackFails ? `Published ${version}: CurseForge file ${fileId}, without its server pack.` : `Published ${version}: CurseForge file ${fileId}, server pack ${fileId + 1}.`,
      () => {
        releases.push({
          version,
          publishedAt: new Date().toISOString(),
          announce: true,
          announcedAt: null,
          displayName: `${pack.name} ${version}`,
          releaseType: config.curseforge.release_type,
          modrinth: config.modrinth.project_id ? modrinthRelease(version) : null,
          modrinthMissing: null,
          serverPackMissing: serverPackFails ? unsentServerPack(version, fileId, 'CurseForge rejected the upload (502 Bad Gateway)') : null,
          curseforge: { projectId: 1234567, fileId, status: 'Not public yet', serverStatus: serverPackFails ? 'Not uploaded' : 'Not public yet', serverPackLinked: null },
        })
      },
    )
  },
  discover_instances: () => [pack, { ...pack, dir: 'C:/Instances/Shared Pack', name: 'Shared Pack', addonCount: 0, hasGit: false, hasLock: false, remote: null, added: false }],
  server_sides: () => serverSides,
  installed_mods: () => ({
    mods: installedMods.map(({ path, projectId, fileId, name, url, logo }) => ({ path, key: `cf:${projectId}`, keys: [`cf:${projectId}`, `mr:${projectId}`], file: `cf:${fileId}`, name, url, logo, authors: authorsOf[projectId] ?? [] })),
    locked: config.updates.locked,
  }),
  set_mods_enabled: (a) =>
    (a.paths as string[]).map((path) => {
      const mod = installedMods.find((m) => m.path === path)!
      mod.path = a.enabled ? mod.path.replace(/\.disabled$/, '') : mod.path.endsWith('.disabled') ? mod.path : `${mod.path}.disabled`
      return mod.path
    }),
  mod_changelog: (a) =>
    new Promise((resolve) =>
      setTimeout(
        () =>
          resolve({
            markdown: false,
            body:
            `<h3>Changes in file ${a.file}</h3><ul><li>Fixed a crash with <strong>Iris</strong> when a shader pack reloads while the player is in a dimension with custom fog</li><li>Updated translations</li></ul>` +
              (Number(String(a.file).replace(/^cf:/, '')) % 2
                ? `<h3>Compatibility</h3><ul>${['Sodium 0.6 and newer', 'Embeddium', 'Create 6', 'Distant Horizons LOD rendering', 'ModernFix dynamic resources', 'FerriteCore'].map((m) => `<li>Works with ${m}</li>`).join('')}</ul><h3>Technical</h3><p>Moved chunk mesh uploads off the render thread and batched them per region, which cuts frame-time spikes on large render distances. Config files from 1.x migrate on first launch; back up <code>config/</code> if you edit them by hand.</p><ul>${Array.from({ length: 8 }, (_, i) => `<li>Internal cleanup ${i + 1}: removed a deprecated API path that other mods no longer call</li>`).join('')}</ul>`
                : '') +
              `<p>Full notes on <a href="https://github.com/example/changelog">GitHub</a>.</p>`,
          }),
        200,
      ),
    ),
  mod_description: (a) =>
    new Promise((resolve) =>
      setTimeout(
        () =>
          resolve({
            markdown: false,
            body: `<h1>About</h1><p class="center"><img src="${logo('A', '#7a4b2a')}" width="64" height="64" alt="Logo"></p><p>${installedMods.find((m) => `cf:${m.projectId}` === a.key)?.name ?? 'This mod'} is a <strong>content mod</strong> with <span style="color: red; text-align: center">styled</span> text. See the <a href="https://github.com/example/wiki">wiki</a>.</p><ul><li>First feature</li><li>Second feature</li></ul><iframe src="https://www.youtube.com/embed/x"></iframe><script>alert(1)</script>`,
          }),
        250,
      ),
    ),
  check_mod_updates: () =>
    new Promise((resolve) =>
      setTimeout(
        () =>
          resolve(
            installedMods.map(({ path, projectId, logo, releaseType, requires, newer }) => ({
              path,
              key: `cf:${projectId}`,
              logo,
              releaseType,
              requires: requires.map((id) => `cf:${id}`),
              newer: newer.map((f, i) => toFile(f, config.modrinth.project_id ? i > 0 || !behindOnModrinth.has(path) : null)),
            })),
          ),
        400,
      ),
    ),
  mod_versions: (a) => {
    const mod = installedMods.find((m) => m.path === a.path)!
    const base = mod.path.slice(mod.path.indexOf('/') + 1).replace(/\.disabled$/, '')
    return [...mod.newer, { fileId: mod.fileId, fileName: base, releaseType: mod.releaseType, fileDate: null as string | null }, { fileId: mod.fileId - 90000, fileName: base.replace(/(\d+)(\.\w+)$/, (_, n, ext) => `${Math.max(0, Number(n) - 1)}${ext}`), releaseType: 'release', fileDate: null }].map((f, i) => ({
      ...toFile({ ...f, fileDate: new Date(Date.UTC(2026, 9, 12 - i * 21)).toISOString() }, config.modrinth.project_id && i === 0 ? !behindOnModrinth.has(mod.path) : null),
    }))
  },
  set_update_channel: (a) => (config.updates.channel = String(a.channel)),
  set_mod_locked: (a) => {
    config.updates.locked = config.updates.locked.filter((key) => key !== a.key)
    if (a.locked) config.updates.locked.push(String(a.key))
  },
  set_version: (a) => {
    const problem = versionProblem(String(a.version))
    if (problem) throw problem
    bcc.version = String(a.version).trim()
  },
  version_problem: (a) => versionProblem(String(a.version)),
  set_up_version_checker: (a) => {
    const placeConfig = () => (bcc.version ??= String(a.version).trim())
    if (bcc.installed) return placeConfig()
    const file = 'BetterCompatibilityChecker-neoforge-5.1.4+1.21.1.jar'
    mockJob('mods', [`Downloading mods/${file}`, `Placed ${file}`], 'Installed Better Compatibility Checker.', () => {
      bcc.installed = true
      placeConfig()
    })
  },
  change_mods: (a) => {
    const requests = (a.requests as { file?: string; project?: string; replaces: string | null }[]).map((r) => ({ fileId: fileOf(r), replaces: r.replaces }))
    // Switching a mod Changes lists for Modrinth swaps it for the version both platforms have.
    const switches = requests.filter((r) => r.replaces && candidates.modrinth[r.replaces]?.skew)
    if (!switches.length) return changeMods(requests)
    return queueModChange(
      switches.map((r) => `Placed ${candidates.modrinth[r.replaces!].skew!.shared?.version}`),
      `Updated ${switches.map((r) => candidates.modrinth[r.replaces!].name).join(', ')}.`,
      () => switches.forEach((r) => delete candidates.modrinth[r.replaces!]),
    )
  },
  cancel_mod_change: (a) => cancelModChange(a.id as number),
  search_mods: (a) => searchMods(a.request as Parameters<typeof searchMods>[0]),
  mod_categories: (a) => (a.kind === 'mods' ? ['Adventure and RPG', 'Magic', 'Storage', 'Technology', 'World Gen'] : ['16x', 'Animated']).map((name, id) => ({ id: String(id), name })),
  // Every other popular project in the catalog is on Modrinth too.
  modrinth_counterparts: (a) =>
    (a.keys as string[]).filter((key) => Number(key.replace(/^cf:/, '')) % 2 === 0).map((key) => ({ key, url: `https://modrinth.com/project/${key.replace(/^cf:/, '')}` })),
  // Every other Modrinth project in the catalog is on CurseForge too.
  curseforge_counterparts: (a) =>
    (a.keys as string[]).filter((key) => Number(key.replace(/^mr:/, '')) % 2 === 0).map((key) => ({ key, url: cfUrl(key.replace(/^mr:/, '')) })),
  preview_mods: (a) => {
    const requests = a.requests as { file?: string; project?: string }[]
    const prefix = requests[0]?.project?.startsWith('mr:') ? 'mr' : 'cf'
    const { wanted, dependencies } = installPlan(requests.map(fileOf))
    const projects = [...wanted.map((p) => `${prefix}:${p.projectId}`), ...dependencies.map((p) => `cf:${p.projectId}`)]
    return new Promise((resolve) => setTimeout(() => resolve({ dependencies: dependencies.map((p) => p.name), unavailable: [], projects }), 300))
  },
  remove_mods: (a) => (installedMods = installedMods.filter((m) => !(a.paths as string[]).includes(m.path))),
  // Shows the browser's confirm in place of the native dialog, and answers with the label of the button you chose.
  'plugin:dialog|message': (a) => {
    const buttons = a.buttons as { OkCancelCustom?: [string, string] } | string | undefined
    const [ok, cancel] = (typeof buttons === 'object' && buttons.OkCancelCustom) || (buttons === 'YesNo' ? ['Yes', 'No'] : ['Ok', 'Cancel'])
    return confirm(`${a.title ? `${a.title}\n\n` : ''}${a.message}`) ? ok : cancel
  },
  discohook_link: () => 'https://discohook.app/?share=example',
  discohook_preview: () => 'https://discohook.app/?share=example',
  get_pack_java: () => null,
  get_run_args: () => runArgs,
  set_run_args: (a) => (runArgs = a.args as typeof runArgs),
  pack_java_default: () => ({ major: 21, path: javaByMajor['21'] ?? mockJavas.find((j) => j.major === 21)?.path ?? null, error: null }),
  scan_java: () => ({
    installs: mockJavas,
    automatic: Object.fromEntries([...new Set(mockJavas.map((j) => j.major))].map((major) => [major, mockJavas.find((j) => j.major === major)!.path])),
    downloads: { 17: 21, 21: 21, 25: 25 },
    needed: [21],
  }),
  // Plays a short download so you can see the progress bar, then adds GraalVM as if it had unpacked.
  download_java: async ({ major }) => {
    const graalvm = major === 25 ? 25 : 21
    const total = 324_662_676
    javaDownloadCancelled = false
    for (let step = 1; step <= 20; step++) {
      await new Promise((resolve) => setTimeout(resolve, 150))
      if (javaDownloadCancelled) throw 'Download cancelled.'
      emit('java-download', { graalvm, received: (total * step) / 20, total })
    }
    const java = { path: `C:/Users/you/AppData/Roaming/dev.muon.packraven/java/graalvm-jdk-${graalvm}.0.4+7.1/bin/java.exe`, major: graalvm, version: `${graalvm}.0.4`, distribution: 'GraalVM', source: 'packraven', foreignArch: null }
    mockJavas.unshift(java)
    if (graalvm !== major) javaByMajor[String(major)] = java.path
    return java
  },
  cancel_java_download: () => {
    javaDownloadCancelled = true
  },
  set_icon_theme: () => null,
  'plugin:window|set_theme': () => null,
  'plugin:autostart|is_enabled': () => false,
  'plugin:event|listen': (a) => {
    listeners.push({ event: String(a.event), handler: Number(a.handler) })
    return Math.floor(Math.random() * 1e9)
  },
  'plugin:event|unlisten': () => null,
}

export function installMock() {
  let callbackId = 0
  const w = window as unknown as Record<string, unknown>
  w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
  w.__mockEmit = emit
  w.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'main' }, currentWebview: { windowLabel: 'main', label: 'main' } },
    transformCallback: (callback: (message: unknown) => void) => {
      callbacks.set(++callbackId, callback)
      return callbackId
    },
    unregisterCallback: () => {},
    convertFileSrc: (path: string) => (/\.(png|jpe?g|gif|webp)$/i.test(path) ? mockImage(path) : path),
    invoke: async (cmd: string, args: Record<string, unknown> = {}) => {
      const handler = handlers[cmd]
      if (handler) return handler(args)
      console.info('[mock] unhandled command', cmd, args)
      return null
    },
  }
}
