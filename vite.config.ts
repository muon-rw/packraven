import { svelte } from '@sveltejs/vite-plugin-svelte'
import { defineConfig } from 'vite'
import { BRIDGE_ENV, packravenBridge } from './vite/bridge.ts'

export default defineConfig({
  plugins: [svelte(), packravenBridge()],
  define: {
    __PACKRAVEN_BRIDGE_HOST__: JSON.stringify(Boolean(process.env[BRIDGE_ENV])),
  },
  // The test server runs beside your own dev server, and two servers sharing one dependency cache
  // rebundle each other's dependencies and reload pages mid-test.
  cacheDir: process.env.PACKRAVEN_VITE_CACHE ?? 'node_modules/.vite',
  // Cargo writes thousands of locked files there during a build, and the frontend imports none of them.
  server: { watch: { ignored: ['**/src-tauri/target*/**'] } },
})
