// Lets a plain browser tab drive a real packraven backend. The scripted instance (`npm run dev:real`)
// loads this module as the host in its webview and runs every Tauri call a browser tab sends it;
// the tab installs the client in place of the mock. `vite/bridge.ts` relays between the two.

declare const __PACKRAVEN_BRIDGE_HOST__: boolean

export const BRIDGE_FILE_PATH = '/__packraven-bridge/file'
const BACKEND_KEY = 'packraven-backend'

type Callback = (message: unknown) => void

interface TauriInternals {
  invoke: (cmd: string, args?: unknown, options?: unknown) => Promise<unknown>
  transformCallback: (callback: Callback, once?: boolean) => number
  unregisterCallback: (id: number) => void
  convertFileSrc: (path: string, protocol?: string) => string
  metadata: unknown
  plugins?: unknown
}

interface HostInfo {
  metadata: unknown
  plugins: unknown
}

const w = window as unknown as Record<string, unknown>

// Tauri returns raw byte responses (`read_bytes`, `git_blob`) as an ArrayBuffer, which JSON can't carry.
function encode(value: unknown): unknown {
  if (value instanceof ArrayBuffer) {
    let binary = ''
    const bytes = new Uint8Array(value)
    for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
    return { __bridgeBytes: btoa(binary) }
  }
  return value
}

function decode(value: unknown): unknown {
  if (value && typeof value === 'object' && '__bridgeBytes' in value) {
    const binary = atob((value as { __bridgeBytes: string }).__bridgeBytes)
    const bytes = new Uint8Array(binary.length)
    for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
    return bytes.buffer
  }
  return value
}

export function wantsRealBackend(): boolean {
  const param = new URLSearchParams(location.search).get('backend')
  if (param === 'real') sessionStorage.setItem(BACKEND_KEY, 'real')
  else if (param) sessionStorage.removeItem(BACKEND_KEY)
  return sessionStorage.getItem(BACKEND_KEY) === 'real'
}

export function isBridgeHost(): boolean {
  return __PACKRAVEN_BRIDGE_HOST__ && '__TAURI_INTERNALS__' in window
}

export function startBridgeHost() {
  const hot = import.meta.hot
  if (!hot) return
  const tauri = w.__TAURI_INTERNALS__ as TauriInternals
  // Per browser tab: the event listeners we registered for it, so we can drop them when it closes.
  const listeners = new Map<string, { event: string; eventId: number; callback: number }[]>()

  async function run(client: string, cmd: string, args: Record<string, unknown> | undefined) {
    if (cmd === 'plugin:event|listen' && args) {
      const handler = args.handler as number
      const callback = tauri.transformCallback((message) => hot!.send('packraven-bridge:event', { client, handler, message }))
      const eventId = (await tauri.invoke(cmd, { ...args, handler: callback })) as number
      const forClient = listeners.get(client) ?? []
      forClient.push({ event: args.event as string, eventId, callback })
      listeners.set(client, forClient)
      return eventId
    }
    if (cmd === 'plugin:event|unlisten' && args) {
      const forClient = listeners.get(client) ?? []
      const index = forClient.findIndex((l) => l.eventId === args.eventId)
      if (index >= 0) tauri.unregisterCallback(forClient.splice(index, 1)[0].callback)
    }
    return tauri.invoke(cmd, args)
  }

  hot.on('packraven-bridge:call', async ({ id, client, cmd, args }: { id: number; client: string; cmd: string; args?: Record<string, unknown> }) => {
    try {
      hot.send('packraven-bridge:result', { id, ok: true, value: encode(await run(client, cmd, args)) })
    } catch (e) {
      hot.send('packraven-bridge:result', { id, ok: false, value: e instanceof Error ? e.message : e })
    }
  })

  hot.on('packraven-bridge:client-gone', ({ client }: { client: string }) => {
    for (const l of listeners.get(client) ?? []) {
      tauri.unregisterCallback(l.callback)
      tauri.invoke('plugin:event|unlisten', { event: l.event, eventId: l.eventId }).catch(() => {})
    }
    listeners.delete(client)
  })

  const hello = () => hot.send('packraven-bridge:host-hello', { metadata: tauri.metadata, plugins: tauri.plugins } satisfies HostInfo)
  hello()
  // Vite reconnects its socket after a server restart, and the relay forgets the host when it does.
  hot.on('vite:ws:connect', hello)
}

/** Resolves once a scripted instance is connected, so the app mounts with a working backend. */
export function installBridgeClient(): Promise<void> {
  const hot = import.meta.hot
  if (!hot) return Promise.reject(new Error('The bridge needs a Vite dev server.'))

  const callbacks = new Map<number, Callback>()
  const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: unknown) => void }>()
  let nextCallback = 0
  let nextCall = 0
  let connected = false

  hot.on('packraven-bridge:result', ({ id, ok, value }: { id: number; ok: boolean; value: unknown }) => {
    const p = pending.get(id)
    if (!p) return
    pending.delete(id)
    if (ok) p.resolve(decode(value))
    else p.reject(value)
  })
  hot.on('packraven-bridge:event', ({ handler, message }: { handler: number; message: unknown }) => callbacks.get(handler)?.(message))
  hot.on('packraven-bridge:host-gone', () => console.warn('[bridge] The scripted instance disconnected. This tab reloads when it returns.'))

  return new Promise((resolve) => {
    hot.on('packraven-bridge:host-ready', (info: HostInfo) => {
      // Listeners registered with the previous instance are gone, so we start over.
      if (connected) return location.reload()
      connected = true
      w.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} }
      w.__TAURI_INTERNALS__ = {
        metadata: info.metadata,
        plugins: info.plugins,
        transformCallback: (callback: Callback, once = false) => {
          const id = ++nextCallback
          callbacks.set(id, once ? (message) => (callbacks.delete(id), callback(message)) : callback)
          return id
        },
        unregisterCallback: (id: number) => callbacks.delete(id),
        convertFileSrc: (path: string) => `${BRIDGE_FILE_PATH}?path=${encodeURIComponent(path)}`,
        invoke: (cmd: string, args: unknown = {}) =>
          new Promise((resolve, reject) => {
            const id = ++nextCall
            pending.set(id, { resolve, reject })
            hot.send('packraven-bridge:call', { id, cmd, args })
          }),
      } satisfies Omit<TauriInternals, 'plugins'> & { plugins: unknown }
      document.getElementById('app')!.textContent = ''
      console.info('[bridge] Connected to the scripted instance.')
      resolve()
    })
    hot.send('packraven-bridge:hello', {})
    document.getElementById('app')!.textContent = 'Waiting for the scripted packraven instance (npm run dev:real)…'
  })
}
