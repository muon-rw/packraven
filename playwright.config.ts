import { existsSync } from 'node:fs'
import { defineConfig } from '@playwright/test'

// Some CI containers ship a Chromium build that may not match this Playwright version,
// so we launch it by path. On Windows we use Edge, which shares its engine with WebView2.
// Anywhere else, run `npx playwright install chromium` once.
const containerChromium = '/opt/pw-browsers/chromium'
const browser = existsSync(containerChromium)
  ? { launchOptions: { executablePath: containerChromium } }
  : process.platform === 'win32'
    ? { channel: 'msedge' }
    : {}

export default defineConfig({
  testDir: 'e2e',
  outputDir: 'test-results',
  fullyParallel: true,
  // On a cold cache, Vite can reload the page mid-test when it finds a new dependency to bundle.
  // Playwright still reports a test that passed on retry as flaky.
  retries: 1,
  reporter: [['list']],
  use: {
    ...browser,
    baseURL: 'http://localhost:5199',
    viewport: { width: 1280, height: 820 },
  },
  projects: [
    { name: 'smoke', testMatch: ['smoke.spec.ts', 'discordMarkdown.spec.ts', 'maven.spec.ts', 'modsEnhancements.spec.ts'] },
    { name: 'shots', testMatch: 'shots.spec.ts' },
  ],
  // Tests get their own port, so the dev server you keep on 5173 (or `npm run tauri dev`) stays yours.
  // A test server already on 5199 is reused.
  webServer: {
    command: 'npm run dev -- --port 5199 --strictPort',
    env: { PACKRAVEN_VITE_CACHE: 'node_modules/.vite-tests' },
    url: 'http://localhost:5199',
    reuseExistingServer: true,
  },
})
