import { expect, test, type Page } from '@playwright/test'
import { openApp } from './app.ts'

async function discordActivity(page: Page): Promise<{ specific: string | null; generic: string | null }> {
  return page.evaluate(() => (window as unknown as { __mockDiscordActivity: () => { specific: string | null; generic: string | null } }).__mockDiscordActivity())
}

test('App settings toggles global Discord status and nests pack visibility underneath', async ({ page }) => {
  await openApp(page, 'settings', 'dirty')

  const globalSwitch = page.getByRole('switch', { name: 'Show packraven in my Discord status' })
  const packSwitch = page.getByRole('switch', { name: "Show the pack I'm working on" })

  await expect(globalSwitch).toHaveAttribute('aria-checked', 'true')
  await expect(packSwitch).toBeVisible()
  await expect(packSwitch).toHaveAttribute('aria-checked', 'true')

  // Turning off pack visibility keeps the global switch on.
  await packSwitch.click()
  await expect(packSwitch).toHaveAttribute('aria-checked', 'false')
  await expect(globalSwitch).toHaveAttribute('aria-checked', 'true')

  // Turning off the global switch keeps the pack switch accessible (so you can disable pack sharing before enabling) while dimming it.
  await globalSwitch.click()
  await expect(globalSwitch).toHaveAttribute('aria-checked', 'false')
  await expect(packSwitch).toBeVisible()
  await expect(packSwitch).toHaveAttribute('aria-checked', 'false')

  // Clicking the label toggles the switch too.
  await page.getByText("Show the pack I'm working on").click()
  await expect(packSwitch).toHaveAttribute('aria-checked', 'true')

  // Turning the global switch back on preserves the updated pack setting.
  await globalSwitch.click()
  await expect(globalSwitch).toHaveAttribute('aria-checked', 'true')
  await expect(packSwitch).toHaveAttribute('aria-checked', 'true')
})

test('reports contextual Discord activity across views and selections', async ({ page }) => {
  await openApp(page, 'changes', 'dirty')
  await expect.poll(() => discordActivity(page)).toEqual({
    specific: 'Writing a changelog',
    generic: 'Writing a changelog',
  })

  // Mods view and expanding a specific mod row.
  await page.getByRole('button', { name: /^Mods\b/ }).click()
  await expect.poll(() => discordActivity(page)).toEqual({
    specific: 'Looking at mods',
    generic: 'Looking at mods',
  })

  const firstModRow = page.locator('table tbody tr').first()
  const selectLabel = await firstModRow.getByRole('checkbox').first().getAttribute('aria-label')
  const modName = selectLabel?.replace(/^Select /, '') ?? ''
  expect(modName.length).toBeGreaterThan(0)
  await firstModRow.click()
  await expect.poll(() => discordActivity(page)).toEqual({
    specific: `Looking at ${modName}`,
    generic: 'Looking at mods',
  })

  // Files view: browsing, then opening a config file.
  await page.getByRole('button', { name: /^Files\b/ }).click()
  await expect.poll(() => discordActivity(page)).toEqual({
    specific: 'Browsing files',
    generic: 'Browsing files',
  })
  const treeFile = page.locator('[role="treeitem"]').filter({ hasText: /\.(toml|json|md|properties|txt)$/ }).first()
  if (await treeFile.count()) {
    const fileName = (await treeFile.innerText()).trim()
    await treeFile.click()
    await expect.poll(() => discordActivity(page)).toEqual({
      specific: `Editing ${fileName}`,
      generic: 'Editing a file',
    })
  }

  // Run view: launching event overrides to Launching the game.
  await page.getByRole('button', { name: /^Run\b/ }).click()
  await expect.poll(() => discordActivity(page)).toEqual({
    specific: 'Getting ready to launch',
    generic: 'Getting ready to launch',
  })
  await page.evaluate(() => {
    const emit = (window as unknown as { __mockEmit: (event: string, payload: unknown) => void }).__mockEmit
    emit('job-state', { job: 'client', state: 'running', message: null })
  })
  await expect.poll(() => discordActivity(page)).toEqual({
    specific: 'Launching the game',
    generic: 'Launching the game',
  })
  await page.evaluate(() => {
    const emit = (window as unknown as { __mockEmit: (event: string, payload: unknown) => void }).__mockEmit
    emit('job-state', { job: 'client', state: 'done', message: null })
  })

  // Publish view includes the version in specific, omits it in generic.
  await page.getByRole('button', { name: /^Publish\b/ }).click()
  await expect.poll(async () => (await discordActivity(page)).generic).toBe('Preparing a release')
  expect((await discordActivity(page)).specific).toMatch(/^Preparing v/)
})
