import { api, errorText, type ClientSlot, type MemoryWarning } from './api'
import { clearLog, clientJob, onServerStopped, serverPack, testOptions, testState } from './jobs.svelte'

// stopRequested tells a stop we asked for apart from a crash, which the backend reports as a normal exit.
export const launch = $state({
  restarting: false,
  restartError: null as string | null,
  stopRequested: false,
  exitedOnItsOwn: false,
  memoryWarning: null as MemoryWarning | null,
})

let pendingSaves: Promise<unknown> = Promise.resolve()

// Settings save on blur, so clicking Start can fire alongside a save. We wait for it so the server
// starts with the new settings and the save can't mark the pack changed after the start cleared it.
export function trackSave<T>(save: Promise<T>): Promise<T> {
  pendingSaves = Promise.all([pendingSaves, save.catch(() => {})])
  return save
}

// The server log shows the test job's lines before the server's, and the backend only clears a
// job's lines once that job starts, so the last run's server log would sit under this one's.
export async function startServer(connect: boolean, secondClient = false) {
  await pendingSaves
  const second = connect && secondClient
  clearLog('test', 'server', ...(connect ? (['client'] as const) : []), ...(second ? (['second-client'] as const) : []))
  if (connect) testState.clientJoins = true
  if (second) testState.secondClientJoins = true
  launch.exitedOnItsOwn = false
  launch.memoryWarning = null
  launch.memoryWarning = await api.startTest(testOptions.recopyAll, connect, second)
  serverPack.changedSinceTest = false
}

export async function launchClient(join: boolean, slot: ClientSlot = 'first') {
  await pendingSaves
  clearLog(clientJob(slot))
  if (slot === 'first') testState.clientJoins = join
  else testState.secondClientJoins = join
  launch.memoryWarning = null
  launch.memoryWarning = await api.startClient(join, slot)
}

export async function stopServer() {
  launch.restarting = false
  launch.stopRequested = true
  try {
    await api.serverCommand('stop')
  } catch (error) {
    launch.stopRequested = false
    throw error
  }
}

export async function killServer() {
  launch.restarting = false
  launch.stopRequested = true
  await api.killProcess('server')
}

// Stop saves the world before the server exits, so we start the next run only once it has.
export async function restartServer() {
  launch.restartError = null
  await stopServer()
  // The server can't have exited yet; vanilla saves every world before it does.
  launch.restarting = true
}

onServerStopped(() => {
  launch.exitedOnItsOwn = !launch.stopRequested
  launch.stopRequested = false
  if (!launch.restarting) return
  launch.restarting = false
  startServer(false).catch((error) => (launch.restartError = errorText(error)))
})

const gigabytes = (bytes: number) => `${(bytes / 2 ** 30).toFixed(1).replace(/\.0$/, '')} GB`

function listed(parts: string[]) {
  return parts.length > 1 ? `${parts.slice(0, -1).join(', ')} and ${parts.at(-1)}` : parts[0]
}

// The backend adds about 1 GB for each Java process beyond its heap, which the last clause accounts for.
export function memoryWarningText(warning: MemoryWarning) {
  const server = warning.launches.find((launch) => launch.process === 'server')
  const clients = warning.launches.filter((launch) => launch.process !== 'server')
  const parts = server ? [`${gigabytes(server.heap)} for the server`] : []
  if (clients.length === 2 && clients[0].heap === clients[1].heap) parts.push(`${gigabytes(clients[0].heap)} for each client`)
  else for (const client of clients) parts.push(`${gigabytes(client.heap)} for the ${client.process === 'secondClient' ? 'second client' : 'client'}`)
  const each = warning.launches.length > 1 ? ' each' : ''
  return `This run can use up to ${gigabytes(warning.needed)} of memory (${listed(parts)}, plus about 1 GB${each} for Java itself), and this computer has ${gigabytes(warning.available)} free. The game may run out of memory, or your computer may slow down. Close other programs, or lower the memory you give the game.`
}

export function memorySummary(warning: MemoryWarning) {
  return `Only ${gigabytes(warning.available)} of memory is free, and this launch can use up to ${gigabytes(warning.needed)}.`
}
