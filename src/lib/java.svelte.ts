import { listen } from '@tauri-apps/api/event'
import { api, errorText, type JavaInstall, type JavaScan, type JavaSource } from './api'

export interface DownloadProgress {
  received: number
  total: number | null
}

export const javas = $state({
  scan: null as JavaScan | null,
  scanning: false,
  error: null as string | null,
  // Keyed by GraalVM version, since Java 17 and Java 21 both download GraalVM 21.
  downloads: {} as Record<number, DownloadProgress>,
  // GraalVM versions we downloaded that Oracle has since published a newer build of.
  outdated: [] as number[],
})

let listening: Promise<unknown> | null = null

function listenForProgress() {
  listening ??= listen<{ graalvm: number } & DownloadProgress>('java-download', ({ payload }) => {
    if (javas.downloads[payload.graalvm]) javas.downloads[payload.graalvm] = { received: payload.received, total: payload.total }
  })
}

export async function scanJava() {
  javas.scanning = true
  try {
    javas.scan = await api.scanJava()
    javas.error = null
    if (javas.scan.installs.some(isOurGraalvm)) checkForUpdates()
  } catch (error) {
    javas.error = errorText(error)
  } finally {
    javas.scanning = false
  }
}

export const isOurGraalvm = (java: JavaInstall) => java.source === 'packraven' && java.distribution === 'GraalVM'

// We only learn about updates when online, so a failed check leaves the last answer in place.
async function checkForUpdates() {
  javas.outdated = await api.outdatedGraalvms().catch(() => javas.outdated)
}

const cancelled = new Set<number>()

// Resolves to null when you cancel.
export async function downloadJava(major: number): Promise<JavaInstall | null> {
  const graalvm = javas.scan?.downloads[major] ?? major
  listenForProgress()
  cancelled.delete(graalvm)
  javas.downloads[graalvm] = { received: 0, total: null }
  try {
    const installed = await api.downloadJava(major)
    await scanJava()
    return installed
  } catch (error) {
    if (cancelled.has(graalvm)) return null
    throw error
  } finally {
    delete javas.downloads[graalvm]
  }
}

export function cancelJavaDownload(graalvm: number) {
  cancelled.add(graalvm)
  api.cancelJavaDownload(graalvm)
}

const SOURCES: Record<JavaSource, string | null> = {
  curseForge: 'CurseForge',
  packraven: 'downloaded by packraven',
  javaHome: 'JAVA_HOME',
  path: 'on PATH',
  system: null,
  intelliJ: 'IntelliJ',
  gradle: 'Gradle',
  prism: 'Prism Launcher',
  modrinth: 'Modrinth App',
}

export function javaName(java: JavaInstall): string {
  return [`${java.distribution} ${java.version}`, SOURCES[java.source], java.foreignArch && `${java.foreignArch} build`].filter(Boolean).join(' · ')
}

export function samePath(a: string, b: string): boolean {
  const normal = (path: string) => path.replaceAll('\\', '/').toLowerCase()
  return normal(a) === normal(b)
}

export function findJava(path: string | null | undefined): JavaInstall | undefined {
  return path ? javas.scan?.installs.find((java) => samePath(java.path, path)) : undefined
}
