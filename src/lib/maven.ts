type Platform = 'curseforge' | 'modrinth'

export function rawProjectId(key: string | null): string | null {
  if (!key) return null
  const sep = key.indexOf(':')
  return sep >= 0 ? key.slice(sep + 1) : key
}

export function rawFileId(file: string | null): string | null {
  if (!file) return null
  const sep = file.indexOf(':')
  return sep >= 0 ? file.slice(sep + 1) : file
}

// Minecraft versions through 1.21.11 are obfuscated; 26.x+ and 1.21.12+ are not.
export function isUpTo12111(minecraft: string | null): boolean {
  if (!minecraft) return false
  const match = minecraft.trim().match(/^(\d+)\.(\d+)(?:\.(\d+))?/)
  if (!match) return false
  const major = Number(match[1])
  const minor = Number(match[2])
  const patch = match[3] ? Number(match[3]) : 0
  if (major < 1) return true
  if (major > 1) return false
  if (minor < 21) return true
  if (minor > 21) return false
  return patch <= 11
}

export function mavenCoordinate(key: string | null, file: string | null, slug: string | null): string | null {
  const projectId = rawProjectId(key)
  const fileId = rawFileId(file)
  if (!key || !projectId || !fileId) return null
  if (key.startsWith('cf:')) {
    if (!slug) return null
    return `curse.maven:${slug}-${projectId}:${fileId}`
  }
  if (key.startsWith('mr:')) {
    return `maven.modrinth:${slug || projectId}:${fileId}`
  }
  return null
}

export function mavenSnippet(params: {
  key: string | null
  file: string | null
  slug: string | null
  minecraft: string | null
  loader: string | null
}): string | null {
  const coord = mavenCoordinate(params.key, params.file, params.slug)
  if (!coord) return null
  const loader = params.loader?.trim().toLowerCase() ?? ''
  if (loader.startsWith('neoforge')) return `implementation("${coord}")`
  if (loader.startsWith('fabric') && isUpTo12111(params.minecraft)) return `modImplementation("${coord}")`
  if (loader.startsWith('forge') && isUpTo12111(params.minecraft)) return `implementation(fg.deobf("${coord}"))`
  return `implementation("${coord}")`
}

export function mavenCopyLabel(platform: Platform): string {
  return platform === 'modrinth' ? 'Copy Modrinth Maven snippet' : 'Copy CurseMaven snippet'
}

export function fileIdCopyLabel(platform: Platform): string {
  return platform === 'modrinth' ? 'Copy version ID' : 'Copy file ID'
}
