import { invoke } from '@tauri-apps/api/core'

export interface Settings {
  instanceDir: string
  packs: string[]
  installDir: string
  javaByMajor: Record<string, string>
  defenderExcluded: string[]
  defenderDismissed: boolean
  minecraftEulaAccepted: boolean
  editorAskMb: number
  editorMaxMb: number
  gitName: string
  gitEmail: string
  autoUpdate: boolean
  discordShowsPack: boolean
  runArgs: RunArgs
}

export type JavaSource = 'curseForge' | 'packraven' | 'javaHome' | 'path' | 'system' | 'intelliJ' | 'gradle' | 'prism' | 'modrinth'

export interface JavaInstall {
  path: string
  major: number
  version: string
  distribution: string
  source: JavaSource
  foreignArch: string | null
}

export interface JavaScan {
  installs: JavaInstall[]
  // The Java packraven picks for each Java version you haven't chosen one for, keyed by major version.
  automatic: Record<string, string>
  // The GraalVM version packraven can download for each Java version.
  downloads: Record<string, number>
  // The Java versions the packs you added need.
  needed: number[]
}

export interface RunArgs {
  clientJvmArgs: string[]
  clientArgs: string[]
  serverJvmArgs: string[]
  serverArgs: string[]
}

export interface PackJavaDefault {
  major: number
  path: string | null
  error: string | null
}

export type UpdatePhase =
  | { phase: 'unchecked' | 'checking' | 'upToDate' }
  | { phase: 'available' | 'ready'; version: string }
  | { phase: 'downloading'; version: string; percent: number | null }
  | { phase: 'failed'; stage: 'check' | 'download' | 'install'; error: string }

export interface UpdateView {
  current: string
  auto: boolean
  supported: boolean
  blocked: string | null
  updatedFrom: string | null
  status: UpdatePhase
}

export interface ReleaseNotes {
  version: string
  publishedAt: string | null
  body: string
}

export interface MinecraftAccount {
  name: string
  uuid: string
}

// packraven.toml lists CurseForge projects by number and Modrinth ones as mr:<id>.
export type StoredModKey = number | string

export interface PackConfig {
  author: string
  curseforge: { project_id: number; release_type: string; manual_release: boolean; allowed_override_jars: string[]; exclude: string[] }
  modrinth: { project_id: string; allowed_override_jars: string[]; exclude: string[]; allowed_authors: string[] }
  export: { exclude: string[] }
  server: {
    exclude: string[]
    client_only: StoredModKey[]
    keep: StoredModKey[]
    memory: string
    jvm_args: string[]
    args: string[]
    graalvm: string
    properties: Record<string, string>
  }
  test: { port: number; client_memory: string }
  discord: { role_id: string; color: string; announce_on_approval: boolean; title: string; show_author: boolean; header: string; footer: string }
  updates: { channel: ReleaseType; locked: StoredModKey[] }
  update_checker: { add_releases: boolean; update_type: UpdateType }
}

export type ReleaseType = 'release' | 'beta' | 'alpha'

export interface UploadNames {
  displayName: string
  fileName: string
}

export interface InstalledMod {
  path: string
  // The project, as cf:<id> or mr:<id>, and the file, as cf:<file id> or mr:<version id>.
  key: string | null
  // The project on every platform packraven knows it from.
  keys: string[]
  file: string | null
  name: string
  url: string | null
  authors: string[]
  logo: string | null
}

export interface ModFile {
  // cf:<file id> or mr:<version id>
  file: string
  fileName: string
  releaseType: ReleaseType
  fileDate: string | null
  // In a pack on Modrinth, whether Modrinth has this CurseForge file too. Null when packraven can't tell.
  onModrinth: boolean | null
}

export interface ModCheck {
  path: string
  key: string
  logo: string | null
  releaseType: ReleaseType | null
  // The keys of the projects this file requires.
  requires: string[]
  newer: ModFile[]
}

// Names a file to place, or a project whose newest fitting file packraven picks.
export type ModRequest = { file: string; replaces: string | null } | { project: string; kind: AddonKind; replaces: null }

export type AddonKind = 'mods' | 'resourcepacks' | 'shaderpacks' | 'datapacks'
export type SearchSort = 'relevance' | 'popularity' | 'downloads' | 'newest' | 'updated'

export interface SearchRequest {
  platform: Platform
  text: string
  kind: AddonKind
  category: string | null
  sort: SearchSort
  index: number
}

export interface SearchResult {
  key: string
  name: string
  summary: string
  authors: string[]
  downloads: number
  updated: string
  logo: string | null
  url: string | null
  // The file packraven would install, when the search says. Modrinth's search doesn't, so packraven picks one when you install.
  file: ModFile | null
}

export interface ModPreview {
  dependencies: string[]
  unavailable: string[]
  // The keys of every project the install would add, its dependencies included.
  projects: string[]
}

// CurseForge writes descriptions and changelogs in HTML, and Modrinth in Markdown.
export interface RemoteText {
  body: string
  markdown: boolean
}

export interface Overview {
  name: string
  version: string | null
  bccInstalled: boolean
  minecraft: string | null
  loader: string | null
  lockCount: number
  addons: { mods: number; resourcePacks: number; shaderPacks: number; dataPacks: number; other: number }
  // Files neither platform the pack uses hosts.
  // Files neither platform hosts, which you need to add to Git or remove.
  unhosted: string[]
  notOnCurseforge: OverrideCandidate[]
  notOnModrinth: OverrideCandidate[]
  modrinthError: string | null
  releasesMissingModrinth: number
  releasesMissingServerPack: number
  modListError: string | null
  watchError: string | null
  branch: string | null
  ahead: number
  behind: number
  incoming: ModCounts | null
  changed: string[]
  lastTag: string | null
  // The version the last release tag belongs to, which isn't always the tag without its v.
  lastVersion: string | null
  devJars: string[]
  projectId: number
  modrinthProjectId: string
  runningJobs: string[]
  serverRunning: boolean
  clientRunning: boolean
  hooks: HooksState
}

export type Platform = 'curseforge' | 'modrinth'

export interface OverrideCandidate {
  path: string
  // The mod the file belongs to, when packraven knows it, so you can check its license.
  name: string | null
  url: string | null
  authors: string[]
  license: string | null
  openLicense: boolean
  // CurseForge's list of the mods it allows in packs includes this one.
  allowlisted: boolean
  // packraven has the project's authors and license, or the file isn't from a project it can ask about.
  known: boolean
  // What Modrinth has of the mod, for a CurseForge file Modrinth doesn't host.
  skew: Skew | null
  versionsKnown: boolean
  // You locked the mod's version in Mods.
  locked: boolean
  // A pattern for this file's other versions, so one choice covers its updates.
  anyVersion: string | null
  // What you decided about another version of this file, which is usually one it replaced.
  earlier: { path: string; choice: OverrideChoice } | null
}

export interface Skew {
  link: { projectId: string; title: string; url: string; evidence: 'sameFiles' | 'sameSource' | 'sameAuthor' }
  // The newest version both platforms have the same build of, as CurseForge's file. `identical` when
  // the files hash the same, otherwise only their contents match.
  shared: { fileId: number; version: string; newer: boolean; identical: boolean } | null
  modrinthNewest: string | null
  modrinthNewer: boolean
  // CurseForge has Modrinth's newest version too.
  newestOnCurseforge: boolean
  // Modrinth's own build of the pack's version, under the same file name, and the entries that differ.
  // When none differ, packraven records it as the file's twin instead, so the file never shows here.
  modrinthBuild: { version: string; differences: string[] } | null
}

export interface OverrideFileView {
  path: string
  name: string | null
  license: string | null
  openLicense: boolean
  allowlisted: boolean
  // The author whose permission ships it, when that's why the pack carries it.
  author: string | null
  // For a choice about every version of a file, the versions in the pack now.
  matches: string[]
  // A pattern for this file's other versions, when the choice is for one file.
  anyVersion: string | null
}

export interface OverrideChoices {
  shipped: OverrideFileView[]
  byAuthor: { author: string; files: OverrideFileView[] }[]
  keptOff: OverrideFileView[]
}

export interface ModCounts {
  added: number
  updated: number
  removed: number
}

export interface ModConflict {
  name: string
  mine: string | null
  theirs: string | null
}

export interface PullPreview {
  incoming: ModCounts
  conflicts: ModConflict[]
}

export type Prefer = 'mine' | 'theirs'

export interface ModChange {
  name: string
  url: string | null
  from: string | null
  to: string | null
  inModList: boolean
}

export interface ModDiff {
  since: string | null
  baseline: boolean
  added: ModChange[]
  updated: ModChange[]
  removed: ModChange[]
}

export type Side = 'both' | 'client' | 'server'
export type Override = 'auto' | 'client' | 'keep'

export interface ModSide {
  path: string
  key: string | null
  name: string
  url: string | null
  side: Side
  reason: string
  override: Override
  requiredBy: string[]
}

export type FileState =
  | 'processing'
  | 'inReview'
  | 'manualReview'
  | 'changesRequired'
  | 'rejected'
  | 'malwareDetected'
  | 'failedPublishing'
  | 'deleted'
  | 'retired'
  | 'awaitingRelease'
  | 'approved'
  | 'other'
export type PackFile = 'client' | 'server'

export type UpdateType = 'minor' | 'minor_breaking' | 'major' | 'incompatible'
export interface UpdateCheckerRepo {
  owner: string
  name: string
  branch: string
  // Empty, or a path ending in a slash.
  folder: string
  // The folder on github.com.
  page: string
}
// forceCompatible: whether Modpack Update Checker still opens worlds from before an incompatible update.
export type UpdateCheckerSetup = { kind: 'github'; repo: UpdateCheckerRepo; forceCompatible: boolean } | { kind: 'unsupported'; reason: string; configPath: string }
export interface UpdateCheckerEntry {
  updateType: UpdateType
  addedAt?: string
  error?: string
}

export interface ReleaseStatus {
  version: string
  publishedAt: string
  announce: boolean
  announcedAt: string | null
  displayName: string | null
  releaseType: string | null
  curseforge: {
    projectId: number
    fileId: number
    status: string
    serverStatus: string
    serverPackLinked: boolean | null
    // What each file's status means for you; null when CurseForge returned no file.
    state: FileState | null
    serverState: FileState | null
    // When each file went up to CurseForge.
    uploaded: string | null
    serverUploaded: string | null
    // Each file's page in CurseForge's author dashboard, where you read its review and can delete it.
    page: string
    serverPage: string | null
  } | null
  modrinth: { status: string; url: string } | null
  // The Modrinth pack packraven would upload, when the pack is on Modrinth but this release isn't yet.
  modrinthMissing: string | null
  serverPackMissing: ServerPackUpload | null
  // Whether the mods changed since the release shipped, so a server pack built now wouldn't match it.
  packChanged: boolean
  // Whether packraven still has the client pack it published, which replacing the client pack reuses.
  clientZipPresent: boolean
  updateChecker: UpdateCheckerEntry | null
}

// What you need to upload a release's server pack yourself, after packraven's upload failed.
export interface ServerPackUpload {
  // The zip packraven built, if it's still in the builds folder.
  zip: string | null
  uploadPage: string
  displayName: string | null
  fileName: string | null
  error: string | null
  // Whether packraven can try the upload again.
  retryable: boolean
  // Whether a retry builds the server pack again first, because the zip is gone or was too large.
  rebuild: boolean
  // Whether the mods changed since the release shipped, so a rebuilt server pack wouldn't match it.
  packChanged: boolean
  // The release's tag, which you check out to rebuild a server pack that's gone from the builds folder.
  tag: string
}

// A release CurseForge or Modrinth is still missing a pack for, read from what packraven recorded.
export interface UnfinishedRelease {
  version: string
  displayName: string | null
  serverPackMissing: ServerPackUpload | null
  // The Modrinth pack packraven built but couldn't upload.
  modrinthMissing: string | null
  modrinthError: string | null
  // The steps after the uploads that packraven hasn't done yet, in order.
  pending: PendingStep[]
  // Whether the mods changed since the release shipped, so tagging it now would record a different mod list.
  packChanged: boolean
}

export type PendingStep = { step: 'tag'; message: string } | { step: 'push' }

export interface PublishedTo {
  curseforge: boolean
  modrinth: boolean
  serverPackMissing: ServerPackUpload | null
  modrinthError: string | null
}

export type OverrideChoice = 'ship' | 'leaveOut'

export type HooksState = 'missing' | 'outdated' | 'current'

export interface TrackedBinary {
  path: string
  kind: 'curseforge' | 'local' | 'generated'
}

export interface InstanceInfo {
  dir: string
  name: string
  minecraft: string | null
  loader: string | null
  addonCount: number
  hasGit: boolean
  hasLock: boolean
  missingMods: number
  hooks: HooksState
  trackedBinaries: TrackedBinary[]
  remote: string | null
  added: boolean
}

export interface PackList {
  active: string | null
  packs: InstanceInfo[]
  instancesRoot: string
}

export interface SetupOptions {
  remote: string | null
  author: string
  projectId: number
  untrackBinaries: boolean
  commit: { message: string; author: string | null } | null
}

export interface FileChange {
  path: string
  origPath: string | null
  status: string
}

export interface GitStatus {
  branch: string | null
  upstream: string | null
  ahead: number
  behind: number
  hasCommits: boolean
  hasRemote: boolean
  staged: FileChange[]
  unstaged: FileChange[]
  conflicted: FileChange[]
  trackedIgnored: string[]
}

export interface GitCommit {
  hash: string
  short: string
  author: string
  when: string
  subject: string
}

export interface GitDiff {
  text: string
  binary: boolean
  truncated: boolean
}

export type LineAction = 'stage' | 'unstage' | 'discard'

export interface DirEntry {
  name: string
  path: string
  isDir: boolean
  size: number
  ignored: boolean
  archive: boolean
  archived: boolean
}

export interface HistoryOutcome {
  label: string
  moved: [string, string][]
  removed: string[]
  added: string[]
  changed: string[]
}

export interface TextQuery {
  pattern: string
  regex: boolean
  matchCase: boolean
  wholeWord: boolean
  include: string
  exclude: string
  includeIgnored: boolean
  replace: string | null
}

/** `column` and `length` place the match in the file; `start` and `end` place it in `preview`. */
export interface TextMatch {
  line: number
  column: number
  length: number
  preview: string
  start: number
  end: number
  replacement: string | null
}

export interface TextResults {
  files: { path: string; matches: TextMatch[] }[]
  total: number
  truncated: boolean
  searched: number
  tooLarge: number
}

export interface Replaced {
  label: string
  files: string[]
  replaced: number
  missed: number
  undoable: boolean
}

export interface Pasted {
  created: string[]
  moved: [string, string][]
}

export interface FileContent {
  text: string | null
  size: number
  unreadable: string | null
  crlf: boolean
  bom: boolean
  archived: boolean
  compressed: boolean
  large: boolean
}

export type PackKind = 'data' | 'resource'

export interface PackEntry {
  path: string
  name: string
  zipped: boolean
  /** Whether options.txt turns on a pack in the instance's resourcepacks folder. */
  enabled?: boolean
}

export interface PackFolder {
  path: string
  exists: boolean
  canCreate: boolean
  packs: PackEntry[]
}

export interface PackLoader {
  id: 'paxi' | 'openloader' | 'globalpacks'
  name: string
  installed: boolean
  projectIds: number[]
  folders: PackFolder[]
}

export interface CopiedIntoPack {
  paths: string[]
  created: string[]
  existing: number
}

export type NbtTag =
  | { type: 'byte' | 'short' | 'int' | 'float' | 'double'; value: number }
  | { type: 'long' | 'string'; value: string }
  | { type: 'byteArray' | 'intArray'; value: { length: number; items: number[] } }
  | { type: 'longArray'; value: { length: number; items: string[] } }
  | { type: 'list'; value: { itemType: string; items: NbtTag[] } }
  | { type: 'compound'; value: [string, NbtTag][] }

export interface NbtDocument {
  compression: 'gzip' | 'zlib' | 'none'
  rootName: string
  root: NbtTag
}

export interface DefenderView {
  active: boolean
  exclusionsReadable: boolean
  missing: string[]
  prompt: boolean
}

export interface IgnoreOption {
  label: string
  patterns: string[]
}

export interface UntrackPreview {
  ignored: string[]
  uncovered: string[]
}

export interface IgnorePreview {
  untracked: string[]
  tracked: string[]
}

export type Credential = 'upload' | 'modrinth' | 'github'
export type CredentialSource = { kind: 'none' } | { kind: 'stored' } | { kind: 'env'; name: string }

export interface CredentialStatus {
  source: CredentialSource
  present: boolean
}

export interface CredentialsView {
  builtInApiKey: boolean
  upload: CredentialStatus
  modrinth: CredentialStatus
  github: CredentialStatus
  keychain: string
}

export const api = {
  gitVersion: () => invoke<string | null>('git_version'),
  installGit: () => invoke<string>('install_git'),
  credentialsStatus: () => invoke<CredentialsView>('credentials_status'),
  credentialStore: (credential: Credential, secret: string) => invoke<void>('credential_store', { credential, secret }),
  credentialUseEnv: (credential: Credential, name: string) => invoke<void>('credential_use_env', { credential, name }),
  credentialClear: (credential: Credential) => invoke<void>('credential_clear', { credential }),
  githubIdentity: () => invoke<{ name: string; email: string }>('github_identity'),
  envVarNames: () => invoke<string[]>('env_var_names'),
  setIconTheme: (dark: boolean) => invoke<void>('set_icon_theme', { dark }),
  getPackJava: () => invoke<string | null>('get_pack_java'),
  setPackJava: (java: string | null) => invoke<void>('set_pack_java', { java }),
  packJavaDefault: () => invoke<PackJavaDefault>('pack_java_default'),
  getRunArgs: () => invoke<RunArgs>('get_run_args'),
  setRunArgs: (args: RunArgs) => invoke<void>('set_run_args', { args }),
  scanJava: () => invoke<JavaScan>('scan_java'),
  downloadJava: (major: number) => invoke<JavaInstall>('download_java', { major }),
  cancelJavaDownload: (graalvm: number) => invoke<void>('cancel_java_download', { graalvm }),
  getServerIcon: () => invoke<string | null>('get_server_icon'),
  setServerIcon: (image: string) => invoke<string | null>('set_server_icon', { image }),
  useProjectLogoAsServerIcon: () => invoke<string | null>('use_project_logo_as_server_icon'),
  removeServerIcon: () => invoke<void>('remove_server_icon'),
  serverLaunch: () => invoke<{ windows: string; unix: string }>('server_launch'),
  getWebhook: () => invoke<string | null>('get_webhook'),
  setWebhook: (url: string) => invoke<void>('set_webhook', { url }),
  discordTest: () => invoke<void>('discord_test'),
  announceRelease: (version: string) => invoke<void>('announce_release', { version }),
  updateCheckerSetup: () => invoke<UpdateCheckerSetup | null>('update_checker_setup'),
  updateCheckerProblem: () => invoke<string | null>('update_checker_problem'),
  addToUpdateChecker: (version: string) => invoke<string>('add_to_update_checker', { version }),
  discohookLink: (version: string) => invoke<string>('discohook_link', { version }),
  discohookPreview: (version: string) => invoke<string>('discohook_preview', { version }),
  defenderStatus: () => invoke<DefenderView>('defender_status'),
  defenderExclude: (paths: string[]) => invoke<void>('defender_exclude', { paths }),
  defenderDismiss: (dismissed: boolean) => invoke<void>('defender_dismiss', { dismissed }),
  acceptMinecraftEula: () => invoke<void>('accept_minecraft_eula'),
  minecraftAccount: () => invoke<MinecraftAccount | null>('minecraft_account'),
  // Resolves to null when you cancel.
  minecraftSignIn: () => invoke<MinecraftAccount | null>('minecraft_sign_in'),
  minecraftSignInCancel: () => invoke<void>('minecraft_sign_in_cancel'),
  minecraftSignOut: () => invoke<void>('minecraft_sign_out'),
  listDir: (path: string) => invoke<DirEntry[]>('list_dir', { path }),
  readFile: (path: string, allowLarge = false) => invoke<FileContent>('read_file', { path, allowLarge }),
  writeFile: (path: string, text: string, crlf: boolean, bom: boolean) => invoke<void>('write_file', { path, text, crlf, bom }),
  absolutePath: (path: string) => invoke<string>('absolute_path', { path }),
  createFile: (path: string) => invoke<void>('create_file', { path }),
  createDir: (path: string) => invoke<void>('create_dir', { path }),
  trashPaths: (paths: string[]) => invoke<void>('trash_paths', { paths }),
  copyFiles: (paths: string[], cut: boolean) => invoke<void>('copy_files', { paths, cut }),
  pasteFiles: (dir: string, cut: string[]) => invoke<Pasted>('paste_files', { dir, cut }),
  openPackFolder: () => invoke<void>('open_pack_folder'),
  openWithDefaultApp: (path: string) => invoke<void>('open_with_default_app', { path }),
  defaultApp: (extension: string) => invoke<string | null>('default_app', { extension }),
  copyPaths: (paths: string[], absolute: boolean) => invoke<void>('copy_paths', { paths, absolute }),
  readBytes: (path: string) => invoke<ArrayBuffer>('read_bytes', { path }),
  readNbt: (path: string) => invoke<NbtDocument>('read_nbt', { path }),
  readLog: (path: string, offset: number) =>
    invoke<{ text: string; offset: number; reset: boolean; truncated: boolean; followable: boolean }>('read_log', { path, offset }),
  decompileClass: (path: string) => invoke<string>('decompile_class', { path }),
  extractArchive: (path: string) => invoke<string>('extract_archive', { path }),
  compressPaths: (paths: string[]) => invoke<string>('compress_paths', { paths }),
  mediaPath: (path: string) => invoke<string>('media_path', { path }),
  packLoaders: (kind: PackKind) => invoke<PackLoader[]>('pack_loaders', { kind }),
  packFolder: (path: string) => invoke<PackFolder>('pack_folder', { path }),
  copyIntoPack: (sources: string[], pack: string, create: boolean) => invoke<CopiedIntoPack>('copy_into_pack', { sources, pack, create }),
  indexFiles: () => invoke<{ paths: string[]; truncated: boolean }>('index_files'),
  searchText: (query: TextQuery) => invoke<TextResults | null>('search_text', { query }),
  replaceText: (query: TextQuery, targets: { path: string; matches: [number, number][] }[]) => invoke<Replaced>('replace_text', { query, targets }),
  importPaths: (dir: string, sources: string[]) => invoke<Pasted>('import_paths', { dir, sources }),
  dragOut: (paths: string[]) => invoke<void>('drag_out', { paths }),
  fileHistory: () => invoke<{ undo: string | null; redo: string | null }>('file_history'),
  fileUndo: (redo: boolean) => invoke<HistoryOutcome | null>('file_undo', { redo }),
  renamePath: (from: string, to: string) => invoke<void>('rename_path', { from, to }),
  renamePaths: (renames: [string, string][]) => invoke<[string, string][]>('rename_paths', { renames }),
  movePaths: (paths: string[], dir: string) => invoke<[string, string][]>('move_paths', { paths, dir }),
  gitOverview: () => invoke<{ status: GitStatus; commits: GitCommit[] }>('git_overview'),
  gitStatus: () => invoke<GitStatus>('git_status'),
  gitDiff: (path: string, staged: boolean, untracked: boolean) => invoke<GitDiff>('git_diff', { path, staged, untracked }),
  gitBlob: (path: string, version: 'head' | 'index') => invoke<ArrayBuffer>('git_blob', { path, version }),
  gitApplyLines: (path: string, diff: string, selected: number[], action: LineAction) => invoke<void>('git_apply_lines', { path, diff, selected, action }),
  gitStage: (paths: string[]) => invoke<void>('git_stage', { paths }),
  gitUnstage: (paths: string[]) => invoke<void>('git_unstage', { paths }),
  gitIgnoreOptions: (paths: string[]) => invoke<IgnoreOption[]>('git_ignore_options', { paths }),
  gitIgnorePreview: (patterns: string[]) => invoke<IgnorePreview>('git_ignore_preview', { patterns }),
  gitIgnore: (patterns: string[]) => invoke<string[]>('git_ignore', { patterns }),
  gitUntrackPreview: (paths: string[]) => invoke<UntrackPreview>('git_untrack_preview', { paths }),
  gitKeepTracking: (paths: string[]) => invoke<string[]>('git_keep_tracking', { paths }),
  gitUntrack: (paths: string[]) => invoke<void>('git_untrack', { paths }),
  gitDiscard: (tracked: string[], untracked: string[]) => invoke<void>('git_discard', { tracked, untracked }),
  gitCommit: (message: string) => invoke<void>('git_commit', { message }),
  gitSync: (action: 'fetch' | 'pull' | 'push', prefer: Prefer | null = null) => invoke<void>('git_sync', { action, prefer }),
  gitPullPreview: () => invoke<PullPreview>('git_pull_preview'),
  listPacks: () => invoke<PackList>('list_packs'),
  discoverInstances: (root?: string) => invoke<InstanceInfo[]>('discover_instances', { root: root ?? null }),
  inspectInstance: (dir: string) => invoke<InstanceInfo>('inspect_instance', { dir }),
  setupInstance: (dir: string, options: SetupOptions) => invoke<void>('setup_instance', { dir, options }),
  joinPack: (dir: string, remote: string) => invoke<void>('join_pack', { dir, remote }),
  switchPack: (dir: string, killTests: boolean) => invoke<void>('switch_pack', { dir, killTests }),
  removePack: (dir: string) => invoke<void>('remove_pack', { dir }),
  updateHooks: () => invoke<void>('update_hooks'),
  getSettings: () => invoke<Settings>('get_settings'),
  saveSettings: (settings: Settings) => invoke<void>('save_settings', { settings }),
  updateStatus: () => invoke<UpdateView>('update_status'),
  checkForUpdate: () => invoke<UpdateView>('check_for_update'),
  installUpdate: () => invoke<void>('install_update'),
  setAutoUpdate: (enabled: boolean) => invoke<void>('set_auto_update', { enabled }),
  setDiscordShowsPack: (enabled: boolean) => invoke<void>('set_discord_shows_pack', { enabled }),
  updateChangelog: (since: string | null = null, through: string | null = null) => invoke<ReleaseNotes[]>('update_changelog', { since, through }),
  dismissUpdateChangelog: () => invoke<void>('dismiss_update_changelog'),
  overview: () => invoke<Overview>('overview'),
  getPackConfig: () => invoke<PackConfig>('get_pack_config'),
  savePackConfig: (config: PackConfig) => invoke<void>('save_pack_config', { config }),
  getNotes: () => invoke<string>('get_notes'),
  saveNotes: (text: string) => invoke<void>('save_notes', { text }),
  changelogPreview: () => invoke<{ diff: ModDiff; markdown: string }>('changelog_preview'),
  setCompareBaseline: (zip: string) => invoke<void>('set_compare_baseline', { zip }),
  clearCompareBaseline: () => invoke<void>('clear_compare_baseline'),
  resyncModList: (fresh: boolean) => invoke<void>('resync_mod_list', { fresh }),
  trackDirectly: (paths: string[]) => invoke<void>('track_directly', { paths }),
  revertMods: () => invoke<void>('revert_mods'),
  serverSides: () => invoke<ModSide[]>('server_sides'),
  setSideOverride: (key: string, mode: Override) => invoke<void>('set_side_override', { key, mode }),
  installedMods: () => invoke<{ mods: InstalledMod[]; locked: string[] }>('installed_mods'),
  checkModUpdates: () => invoke<ModCheck[]>('check_mod_updates'),
  modVersions: (path: string, key: string) => invoke<ModFile[]>('mod_versions', { path, key }),
  setUpdateChannel: (channel: ReleaseType) => invoke<void>('set_update_channel', { channel }),
  searchMods: (request: SearchRequest) => invoke<{ results: SearchResult[]; nextIndex: number | null }>('search_mods', { request }),
  modCategories: (platform: Platform, kind: AddonKind) => invoke<{ id: string; name: string }[]>('mod_categories', { platform, kind }),
  modrinthCounterparts: (keys: string[]) => invoke<{ key: string; url: string }[]>('modrinth_counterparts', { keys }),
  curseforgeCounterparts: (keys: string[]) => invoke<{ key: string; url: string }[]>('curseforge_counterparts', { keys }),
  previewMods: (requests: ModRequest[]) => invoke<ModPreview>('preview_mods', { requests }),
  changeMods: (requests: ModRequest[]) => invoke<number>('change_mods', { requests }),
  cancelModChange: (id: number) => invoke<boolean>('cancel_mod_change', { id }),
  setVersion: (version: string) => invoke<void>('set_version', { version }),
  versionProblem: (version: string) => invoke<string | null>('version_problem', { version }),
  setUpVersionChecker: (version: string) => invoke<void>('set_up_version_checker', { version }),
  setModLocked: (key: string, locked: boolean) => invoke<void>('set_mod_locked', { key, locked }),
  setModsEnabled: (paths: string[], enabled: boolean) => invoke<string[]>('set_mods_enabled', { paths, enabled }),
  modChangelog: (key: string, file: string) => invoke<RemoteText>('mod_changelog', { key, file }),
  modDescription: (key: string) => invoke<RemoteText>('mod_description', { key }),
  removeMods: (paths: string[]) => invoke<void>('remove_mods', { paths }),
  publishPreflight: (version: string) =>
    invoke<{ problems: string[]; warnings: string[]; tagTaken: boolean; client: UploadNames; server: UploadNames; modrinthFile: string; curseforgeShipped: OverrideFileView[]; curseforgeLeftOut: OverrideFileView[]; modrinthShipped: OverrideFileView[]; modrinthLeftOut: OverrideFileView[] }>('publish_preflight', { version }),
  buildClientExport: () => invoke<void>('build_client_export'),
  buildModrinthExport: () => invoke<void>('build_modrinth_export'),
  uploadReleaseToModrinth: (version: string) => invoke<void>('upload_release_to_modrinth', { version }),
  uploadReleaseServerPack: (version: string) => invoke<void>('upload_release_server_pack', { version }),
  finishRelease: (version: string) => invoke<void>('finish_release', { version }),
  markServerPackUploaded: (version: string) => invoke<void>('mark_server_pack_uploaded', { version }),
  replaceCurseforgeFile: (version: string, file: PackFile) => invoke<void>('replace_curseforge_file', { version, file }),
  releaseAttention: (version: string) => invoke<{ file: PackFile; state: FileState } | null>('release_attention', { version }),
  setOverrideChoice: (platform: Platform, paths: string[], choice: OverrideChoice | null) => invoke<void>('set_override_choice', { platform, paths, choice }),
  overrideChoices: (platform: Platform) => invoke<OverrideChoices>('override_choices', { platform }),
  lookUpOverrideCandidates: () => invoke<void>('look_up_override_candidates'),
  setModrinthAuthor: (author: string, allowed: boolean) => invoke<void>('set_modrinth_author', { author, allowed }),
  useModrinthBuild: (path: string, using: boolean) => invoke<void>('use_modrinth_build', { path, using }),
  modrinthProjectTitle: (id: string) => invoke<string | null>('modrinth_project_title', { id }),
  publishedTo: (version: string) => invoke<PublishedTo | null>('published_to', { version }),
  unfinishedReleases: () => invoke<UnfinishedRelease[]>('unfinished_releases'),
  buildServerPack: () => invoke<void>('build_server_pack'),
  startTest: (recopyAll: boolean, connect: boolean) => invoke<void>('start_test', { recopyAll, connect }),
  startClient: (join: boolean) => invoke<void>('start_client', { join }),
  serverCommand: (line: string) => invoke<void>('server_command', { line }),
  killProcess: (which: 'server' | 'client') => invoke<void>('kill_process', { which }),
  publish: (options: { version: string; message: string; push: boolean; client: UploadNames; server: UploadNames; updateChecker: UpdateType | null }) => invoke<void>('publish', { options }),
  releases: () => invoke<ReleaseStatus[]>('releases'),
  outputDir: () => invoke<string>('output_dir'),
}

export function errorText(error: unknown): string {
  return typeof error === 'string' ? error : error instanceof Error ? error.message : JSON.stringify(error)
}
