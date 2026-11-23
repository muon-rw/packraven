// Starts a scripted packraven debug build whose webview serves the browser bridge, so you can open
// http://localhost:5199/?backend=real in any browser and drive the real backend. It runs beside
// our own `npm run tauri dev` on 5173: it builds into its own Cargo target directory (Windows
// locks a running exe, which would fail a shared build) and skips the single-instance lock.
//
//   npm run dev:real          start it (the first build takes a few minutes)
//   npm run dev:real -- --pack ATM10
//                             open a pack you've added, by path or part of its folder name
//   npm run dev:real -- stop  stop it and anything it left behind

import { execSync, spawn } from 'node:child_process'
import { createServer } from 'node:net'
import { fileURLToPath } from 'node:url'

const PORT = 5199
const root = fileURLToPath(new URL('..', import.meta.url))
const windows = process.platform === 'win32'

function stop() {
  const script = windows
    ? `Get-Process packraven -ErrorAction SilentlyContinue | Where-Object { $_.Path -like '*target-scripted*' } | Stop-Process -Force;` +
      `Get-NetTCPConnection -State Listen -LocalPort ${PORT} -ErrorAction SilentlyContinue | ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }`
    : `pkill -f target-scripted; lsof -ti tcp:${PORT} -s tcp:listen | xargs -r kill`
  try {
    execSync(script, { stdio: 'inherit', shell: windows ? 'powershell.exe' : '/bin/sh' })
  } catch {
    // Nothing was running.
  }
}

function portIsFree() {
  return new Promise((resolve) => {
    const server = createServer()
      .once('error', () => resolve(false))
      .once('listening', () => server.close(() => resolve(true)))
      .listen(PORT, 'localhost')
  })
}

const packFlag = process.argv.indexOf('--pack')
const pack = packFlag >= 0 ? process.argv[packFlag + 1] : undefined

if (process.argv.includes('stop')) {
  stop()
  process.exit(0)
}

if (!(await portIsFree())) {
  console.error(`Port ${PORT} is taken, probably by a test or mock dev server. Stop it (or run \`npm run dev:real -- stop\`) and try again.`)
  process.exit(1)
}

// One command string, since Node deprecates passing an argument list along with `shell`.
const child = spawn('npx tauri dev --config src-tauri/tauri.scripted.conf.json', {
  cwd: root,
  stdio: 'inherit',
  shell: true,
  env: {
    ...process.env,
    PACKRAVEN_SCRIPTED_INSTANCE: '1',
    ...(pack ? { PACKRAVEN_SCRIPTED_PACK: pack } : {}),
    PACKRAVEN_VITE_CACHE: 'node_modules/.vite-tests',
    CARGO_TARGET_DIR: `${root}src-tauri/target-scripted`,
  },
})

const shutdown = () => {
  stop()
  process.exit(0)
}
process.on('SIGINT', shutdown)
process.on('SIGTERM', shutdown)
child.on('exit', (code) => {
  stop()
  process.exit(code ?? 0)
})
