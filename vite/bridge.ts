import { createReadStream, statSync } from 'node:fs'
import { extname } from 'node:path'
import type { Plugin, WebSocketClient } from 'vite'

// Relays a browser tab's Tauri calls to a scripted packraven instance (`npm run dev:real`), whose
// webview runs `src/dev/bridge.ts` as the host. Both sides use Vite's HMR socket, so the
// relay doesn't need a port of its own. Open `http://localhost:5199/?backend=real` to use.

export const BRIDGE_ENV = 'PACKRAVEN_SCRIPTED_INSTANCE'
export const BRIDGE_FILE_PATH = '/__packraven-bridge/file'

const MIME: Record<string, string> = {
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.gif': 'image/gif',
  '.webp': 'image/webp',
  '.svg': 'image/svg+xml',
  '.mp4': 'video/mp4',
  '.webm': 'video/webm',
  '.ogg': 'audio/ogg',
  '.mp3': 'audio/mpeg',
  '.wav': 'audio/wav',
}

interface Call {
  id: number
  cmd: string
  args: unknown
}

interface Pending {
  client: WebSocketClient
  id: number
}

export function packravenBridge(): Plugin {
  return {
    name: 'packraven-bridge',
    apply: 'serve',
    configureServer(server) {
      if (!process.env[BRIDGE_ENV]) return

      let host: WebSocketClient | undefined
      let hostInfo: unknown
      const browsers = new Map<string, WebSocketClient>()
      const keys = new WeakMap<WebSocketClient, string>()
      const pending = new Map<number, Pending>()
      let nextKey = 0
      let nextCall = 0

      const keyOf = (client: WebSocketClient) => {
        let key = keys.get(client)
        if (!key) {
          key = String(++nextKey)
          keys.set(client, key)
          browsers.set(key, client)
          client.socket.once('close', () => {
            browsers.delete(key!)
            host?.send('packraven-bridge:client-gone', { client: key })
          })
        }
        return key
      }

      server.ws.on('packraven-bridge:host-hello', (info: unknown, client) => {
        host = client
        hostInfo = info
        client.socket.once('close', () => {
          if (host !== client) return
          host = undefined
          for (const [id, p] of pending) {
            p.client.send('packraven-bridge:result', { id: p.id, ok: false, value: 'The scripted instance reloaded or exited during this call.' })
            pending.delete(id)
          }
          for (const browser of browsers.values()) browser.send('packraven-bridge:host-gone', {})
        })
        for (const browser of browsers.values()) browser.send('packraven-bridge:host-ready', hostInfo)
      })

      server.ws.on('packraven-bridge:hello', (_data, client) => {
        keyOf(client)
        if (host) client.send('packraven-bridge:host-ready', hostInfo)
      })

      server.ws.on('packraven-bridge:call', (call: Call, client) => {
        if (!host) {
          client.send('packraven-bridge:result', { id: call.id, ok: false, value: 'No scripted instance is connected. Start one with `npm run dev:real`.' })
          return
        }
        const id = ++nextCall
        pending.set(id, { client, id: call.id })
        host.send('packraven-bridge:call', { id, client: keyOf(client), cmd: call.cmd, args: call.args })
      })

      server.ws.on('packraven-bridge:result', (result: { id: number; ok: boolean; value: unknown }) => {
        const p = pending.get(result.id)
        if (!p) return
        pending.delete(result.id)
        p.client.send('packraven-bridge:result', { ...result, id: p.id })
      })

      server.ws.on('packraven-bridge:event', (event: { client: string; handler: number; message: unknown }) => {
        browsers.get(event.client)?.send('packraven-bridge:event', { handler: event.handler, message: event.message })
      })

      // Stands in for Tauri's asset protocol, which only works inside the webview.
      server.middlewares.use(BRIDGE_FILE_PATH, (req, res) => {
        // Any page you open can request localhost URLs, so we only serve the app's own pages.
        const site = req.headers['sec-fetch-site']
        if (site && site !== 'same-origin' && site !== 'none') {
          res.statusCode = 403
          res.end()
          return
        }
        const path = new URL(req.url ?? '', 'http://localhost').searchParams.get('path')
        try {
          if (!path || !statSync(path).isFile()) throw new Error('not a file')
        } catch {
          res.statusCode = 404
          res.end()
          return
        }
        res.setHeader('Content-Type', MIME[extname(path).toLowerCase()] ?? 'application/octet-stream')
        res.setHeader('Cache-Control', 'no-store')
        createReadStream(path).pipe(res)
      })
    },
  }
}
