import { expect, test } from '@playwright/test'
import { openApp, packViews, scenarios, type Scenario, type View } from './app.ts'

const cases: [View, Scenario][] = [...scenarios.filter((s) => s !== 'nopack').flatMap((s) => packViews.map((v): [View, Scenario] => [v, s])), ['add', 'nopack']]

for (const [view, scenario] of cases) {
  test(`${view} renders in ${scenario}`, async ({ page }) => {
    const problems: string[] = []
    page.on('pageerror', (error) => problems.push(`uncaught: ${error.message}`))
    page.on('console', (message) => {
      // Browsers request a favicon on their own; the Tauri webview doesn't.
      if (message.location().url.endsWith('/favicon.ico')) return
      // The mock logs commands it has no handler for. Views must not need one just to render.
      if (message.type() === 'error' || message.text().startsWith('[mock] unhandled command')) problems.push(message.text())
    })
    await openApp(page, view, scenario)
    expect(problems).toEqual([])
  })
}

test('installed mods can be sorted, filtered, expanded and changed', async ({ page }) => {
  await openApp(page, 'mods', 'dirty')
  const rows = page.getByRole('region', { name: 'Installed mods' }).locator('tbody > tr.row')
  await expect(page.getByRole('button', { name: 'Update all (5)' })).toBeEnabled()
  await expect(page.getByRole('status').filter({ hasText: 'two versions of Iris Shaders' })).toBeVisible()

  // Mods with updates come first until you pick another sort.
  await expect(rows.first().getByRole('button', { name: /^Update / })).toBeVisible()
  await page.getByRole('button', { name: 'Author' }).click()
  await expect(rows.first()).toContainText('coderbot')
  await page.getByRole('button', { name: 'Update', exact: true }).click()

  await page.getByRole('button', { name: /^Resource packs/ }).click()
  await expect(rows).toHaveCount(1)
  const freshAnimations = page.getByRole('switch', { name: 'Enable Fresh Animations' })
  await freshAnimations.click()
  await expect(freshAnimations).toHaveAttribute('aria-checked', 'false')
  await page.getByRole('button', { name: /^Data packs/ }).click()
  await expect(page.getByRole('region', { name: 'Installed mods' })).toContainText('no data packs yet')
  await page.getByRole('group', { name: 'Show' }).getByRole('button', { name: /^Mods/ }).click()

  // Clicking a row away from its controls opens its versions and description.
  await rows.filter({ hasText: 'Apotheosis' }).locator('td.author').click()
  await expect(page.getByRole('list', { name: 'Versions of Apotheosis' }).getByRole('listitem')).toHaveCount(3)
  await page.getByRole('button', { name: 'Description', exact: true }).click()
  const description = page.locator('.description')
  await expect(description).toContainText('content mod')
  await expect(description.locator('iframe, script')).toHaveCount(0)
  await expect(description.locator('span')).not.toHaveAttribute('style', /color/)

  await page.getByRole('button', { name: 'Update Apotheosis', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: 'Updated Apotheosis.' })).toBeVisible()

  // Update all opens a review, where disabled mods start unticked.
  await page.getByRole('button', { name: 'Update all (4)' }).click()
  const review = page.getByRole('dialog', { name: 'Review 4 updates' })
  await expect(review.getByRole('checkbox', { name: /Better Clouds/ })).not.toBeChecked()
  await expect(review.getByRole('checkbox', { name: /Sodium/ })).toBeChecked()
  await review.getByRole('button', { name: 'Changelog' }).first().click()
  await expect(review).toContainText('Fixed a crash with Iris')
  await review.getByRole('checkbox', { name: /Sodium/ }).uncheck()
  await review.getByRole('button', { name: 'Update 2 files' }).click()
  await expect(page.getByRole('status').filter({ hasText: /Updated .*Iron's Spells/ })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Update Sodium', exact: true })).toBeVisible()

  const betterClouds = page.getByRole('switch', { name: 'Enable Better Clouds' })
  await expect(betterClouds).toHaveAttribute('aria-checked', 'false')
  await betterClouds.click()
  await expect(betterClouds).toHaveAttribute('aria-checked', 'true')

  // Scrolling closes the menu, so we bring the row into view before right-clicking it.
  await rows.filter({ hasText: 'Placebo' }).scrollIntoViewIfNeeded()
  await rows.filter({ hasText: 'Placebo' }).click({ button: 'right' })
  await expect(page.getByRole('menuitem', { name: 'Copy link' })).toBeVisible()
  await page.keyboard.press('Escape')

  page.once('dialog', (dialog) => {
    expect(dialog.message()).toContain("Apotheosis and Iron's Spells 'n Spellbooks require Placebo, directly or through another mod")
    dialog.accept()
  })
  await page.getByRole('button', { name: 'Remove Placebo' }).click()
  await expect(page.getByRole('button', { name: 'Remove Placebo' })).toHaveCount(0)
})

test('search counts matches on every type tab', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('searchbox', { name: 'Find a mod or author' }).fill('complementary')
  const region = page.getByRole('region', { name: 'Installed mods' })
  await expect(region).toContainText('No mods match "complementary"')
  await expect(page.getByRole('group', { name: 'Show' }).getByRole('button', { name: /^Shaders/ })).toContainText('1')
  await region.getByRole('button', { name: 'Show 1 in Shaders' }).click()
  await expect(region.locator('tbody > tr.row')).toHaveCount(1)
})

test('selected mods can be disabled together and brought back', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('checkbox', { name: 'Select Just Enough Items (JEI)' }).check()
  await page.getByRole('checkbox', { name: 'Select Shoulder Surfing Reloaded' }).check()
  const bar = page.getByRole('toolbar', { name: 'Selected files' })
  await expect(bar).toContainText('2 selected')
  await bar.getByRole('button', { name: 'Disable' }).click()
  for (const name of ['Just Enough Items (JEI)', 'Shoulder Surfing Reloaded']) {
    await expect(page.getByRole('switch', { name: `Enable ${name}` })).toHaveAttribute('aria-checked', 'false')
  }
  await page.getByRole('status').getByRole('button', { name: 'Undo' }).click()
  await expect(page.getByRole('switch', { name: 'Enable Just Enough Items (JEI)' })).toHaveAttribute('aria-checked', 'true')

  // An older file is a rollback, and its button says so.
  await page.getByRole('button', { name: 'Placebo', exact: true }).click()
  await expect(page.getByRole('list', { name: 'Versions of Placebo' }).getByRole('button', { name: /^Roll back to/ })).toHaveCount(1)
})

test('mods can be found on CurseForge and installed with their dependencies', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('button', { name: 'Add from CurseForge' }).click()
  await expect(page.getByRole('region', { name: 'Results from CurseForge' }).getByText('Installed', { exact: true })).not.toHaveCount(0)

  await page.getByRole('searchbox', { name: 'Search CurseForge' }).fill('steam')
  await expect(page.getByRole('button', { name: /^Install / })).toHaveCount(1)
  await page.getByRole('button', { name: "Install Create: Steam 'n' Rails" }).click()
  await expect(page.getByRole('group', { name: "Install Create: Steam 'n' Rails" })).toContainText('requires Create and Flywheel')
  await page.getByRole('button', { name: 'Install all 3' }).click()
  await expect(page.getByRole('button', { name: /installing Create: Steam 'n' Rails/i })).toContainText('Installing')

  await expect(page.getByRole('status').filter({ hasText: "Installed Create: Steam 'n' Rails. Added Create and Flywheel as required." })).toBeVisible()
  await expect(page.getByRole('button', { name: /^Install / })).toHaveCount(0)
  await expect(page.getByRole('button', { name: /^Back to installed/ })).toContainText('13')
})

test('mods can be added from Modrinth, and Escape goes back to the installed list', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('button', { name: 'Add from Modrinth' }).click()
  await expect(page.getByRole('heading', { name: 'Add from Modrinth' })).toBeVisible()
  await expect(page.getByRole('searchbox', { name: 'Search Modrinth' })).toBeFocused()
  await page.getByRole('searchbox', { name: 'Search Modrinth' }).fill('steam')
  await page.getByRole('button', { name: "Install Create: Steam 'n' Rails" }).click()
  const prompt = page.getByRole('group', { name: "Install Create: Steam 'n' Rails" })
  await expect(prompt).toContainText("We didn't find Create: Steam 'n' Rails on CurseForge")
  await expect(prompt).toContainText('requires Create and Flywheel')
  // Escape closes the prompt first, and stays in the search.
  await page.keyboard.press('Escape')
  await expect(prompt).toHaveCount(0)
  await expect(page.getByRole('button', { name: "Install Create: Steam 'n' Rails" })).toBeFocused()
  await expect(page.getByRole('heading', { name: 'Add from Modrinth' })).toBeVisible()

  await page.getByRole('searchbox', { name: 'Search Modrinth' }).fill('')
  await page.keyboard.press('Escape')
  await expect(page.getByRole('heading', { name: 'Mods' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Add from Modrinth' })).toBeFocused()
})

test('a pack with no projects yet can add from either platform', async ({ page }) => {
  await openApp(page, 'mods', 'empty')
  await page.locator('#mods-add-modrinth').click()
  await expect(page.getByRole('heading', { name: 'Add from Modrinth' })).toBeVisible()
  await expect(page.getByRole('searchbox', { name: 'Search Modrinth' })).toBeFocused()
})

test("an update Modrinth doesn't have yet waits, and says why", async ({ page }) => {
  await openApp(page, 'mods', 'dirty')
  await expect(page.getByText("isn't on Modrinth yet")).toBeVisible()
})

test('updates you click during a download join it', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('button', { name: 'Update Apotheosis', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Updating Apotheosis' })).toHaveAttribute('aria-disabled', 'true')
  await expect(page.getByRole('button', { name: 'Updating Apotheosis' })).toBeFocused()
  await expect(page.getByRole('status').filter({ hasText: 'Downloading 1 file.' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Update all (4)' })).toBeEnabled()

  await page.getByRole('button', { name: 'Update Sodium', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Cancel updating Sodium' })).toBeVisible()

  await expect(page.getByRole('status').filter({ hasText: 'Updated Apotheosis. Updated Sodium.' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Update all (3)' })).toBeEnabled()
})

test('updated mods leave the top of the Update sort when the run ends', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  const firstRow = page.locator('tbody > tr.row .expand').first()
  await expect(firstRow).toHaveText('Apotheosis')
  await page.getByRole('button', { name: 'Update Apotheosis', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: 'Updated Apotheosis.' })).toBeVisible()
  await expect(firstRow).not.toHaveText('Apotheosis')
})

test('a queued update can be cancelled before it starts', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('button', { name: 'Update Apotheosis', exact: true }).click()
  await page.getByRole('button', { name: 'Update Sodium', exact: true }).click()
  await page.getByRole('button', { name: 'Cancel updating Sodium' }).click()
  await expect(page.getByRole('button', { name: 'Update Sodium', exact: true })).toBeVisible()
  await expect(page.getByRole('status').filter({ hasText: 'Updated Apotheosis. Cancelled updating Sodium.' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Update Sodium', exact: true })).toBeVisible()
})

test("a change can't be cancelled once it starts", async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('button', { name: 'Update Apotheosis', exact: true }).click()
  await page.getByRole('button', { name: 'Update Sodium', exact: true }).click()
  const running = page.getByRole('button', { name: 'Updating Apotheosis' })
  await expect(running).toHaveAttribute('aria-disabled', 'true')
  await running.click({ force: true })
  // We only reload the list when the run ends, so until then the row says what happened.
  const updated = page.getByRole('button', { name: 'Updated Apotheosis' })
  await expect(updated).toHaveAttribute('aria-disabled', 'true')
  await expect(page.getByRole('status').filter({ hasText: 'Updated Apotheosis. Updated Sodium.' })).toBeVisible()
})

test('cancelling one mod of a queued batch cancels the batch', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('button', { name: 'Update Apotheosis', exact: true }).click()
  await page.getByRole('button', { name: 'Update all (4)' }).click()
  await page.getByRole('dialog').getByRole('button', { name: /^Update \d+ files$/ }).click()
  const cancel = page.getByRole('button', { name: 'Cancel updating Sodium' })
  await expect(cancel).toHaveAttribute('title', /^Cancel this update\. It also cancels /)
  // Pointing at one row's Cancel marks every row it would cancel.
  await cancel.hover()
  await expect(page.getByRole('button', { name: "Cancel updating Iron's Spells 'n Spellbooks" })).toHaveClass(/pointed/)
  await cancel.click()
  await expect(page.getByRole('button', { name: 'Update Sodium', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: "Update Iron's Spells 'n Spellbooks", exact: true })).toBeVisible()
  await expect(page.getByRole('status').filter({ hasText: /Updated Apotheosis\. Cancelled updating Sodium and \d+ other mods?\./ })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Update all (4)' })).toBeEnabled()
})

test('a locked mod stays out of Update all', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  const lock = page.getByRole('button', { name: 'Lock Apotheosis to this version' })
  await expect(page.getByRole('button', { name: 'Update all (5)' })).toBeEnabled()

  await lock.click()
  await expect(lock).toHaveAttribute('aria-pressed', 'true')
  await expect(page.getByRole('button', { name: 'Update all (4)' })).toBeEnabled()
  await expect(page.getByRole('button', { name: 'Update Apotheosis', exact: true })).toHaveCount(0)
  await expect(page.getByText('Locked; 8.9.0 is available')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Update Apotheosis, locked' })).toHaveAttribute('aria-disabled', 'true')

  await page.getByRole('button', { name: 'Locked', exact: true }).click()
  await expect(page.getByRole('region', { name: 'Installed mods' }).locator('tbody > tr.row').first()).toContainText('Apotheosis')

  await lock.click()
  await expect(page.getByRole('button', { name: 'Update all (5)' })).toBeEnabled()
})

test('disabling a mod others need offers to disable them too', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  const placebo = page.getByRole('switch', { name: 'Enable Placebo' })
  await expect(page.getByRole('button', { name: 'Update all (5)' })).toBeEnabled()

  await placebo.click()
  const dialog = page.getByRole('dialog', { name: 'Disable Placebo' })
  await expect(dialog.getByRole('list', { name: 'Mods that require Placebo' }).getByRole('listitem')).toHaveText(['Apotheosis', "Iron's Spells 'n Spellbooks"])
  await expect(dialog.getByRole('checkbox')).toBeChecked()
  await dialog.getByRole('button', { name: 'Disable 3 files' }).click()
  for (const name of ['Placebo', 'Apotheosis', "Iron's Spells 'n Spellbooks"]) {
    await expect(page.getByRole('switch', { name: `Enable ${name}` })).toHaveAttribute('aria-checked', 'false')
  }

  // Turning dependents back on is a choice per mod, so enabling Placebo leaves the others off.
  await placebo.click()
  await expect(placebo).toHaveAttribute('aria-checked', 'true')
  await expect(page.getByRole('switch', { name: 'Enable Apotheosis' })).toHaveAttribute('aria-checked', 'false')

  await page.getByRole('switch', { name: 'Enable Apotheosis' }).click()
  await page.getByRole('switch', { name: "Enable Iron's Spells 'n Spellbooks" }).click()
  await placebo.click()
  await dialog.getByRole('checkbox').uncheck()
  await dialog.getByRole('button', { name: 'Disable Placebo only' }).click()
  await expect(placebo).toHaveAttribute('aria-checked', 'false')
  await expect(page.getByRole('switch', { name: 'Enable Apotheosis' })).toHaveAttribute('aria-checked', 'true')
})

test('clicking a version shows its changelog', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('button', { name: 'Apotheosis', exact: true }).click()
  const versions = page.getByRole('list', { name: 'Versions of Apotheosis' })
  const picks = versions.locator('button.pick')
  const changelog = page.getByRole('region', { name: /^Changelog of / })
  // The newest file's changelog shows first, since that's the one you'd update to.
  await expect(picks.first()).toHaveAttribute('aria-current', 'true')
  await expect(changelog).toContainText('Fixed a crash with Iris')
  const fileName = (await picks.nth(1).locator('.visually-hidden').textContent())!
  await picks.nth(1).click()
  await expect(picks.nth(1)).toHaveAttribute('aria-current', 'true')
  await expect(picks.first()).not.toHaveAttribute('aria-current')
  await expect(changelog).toHaveAccessibleName(`Changelog of ${fileName}`)
  // Arrow keys move the selection, and only the selected version takes a Tab stop.
  await page.keyboard.press('ArrowDown')
  await expect(picks.nth(2)).toBeFocused()
  await expect(picks.nth(2)).toHaveAttribute('aria-current', 'true')
  await expect(picks.nth(1)).toHaveAttribute('tabindex', '-1')
})

test('clicking a Browse result shows its description in place', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('button', { name: 'Add from CurseForge' }).click()
  const name = page.getByRole('button', { name: 'Create', exact: true })
  await name.click()
  await expect(name).toHaveAttribute('aria-expanded', 'true')
  await expect(page.getByRole('region', { name: 'Description of Create' })).toContainText('content mod')
  await name.click()
  await expect(page.getByRole('region', { name: 'Description of Create' })).toHaveCount(0)
})

test('test servers wait until you accept the Minecraft EULA', async ({ page }) => {
  await openApp(page, 'test', 'empty')
  const consent = page.getByRole('status').filter({ hasText: 'Accept the Minecraft EULA' })
  await expect(consent.getByRole('link', { name: 'Read the Minecraft EULA' })).toHaveAttribute('href', 'https://aka.ms/MinecraftEULA')
  for (const name of ['Quick test', 'Start server']) {
    await expect(page.getByRole('button', { name, exact: true })).toHaveAttribute('aria-disabled', 'true')
  }

  await consent.getByRole('button', { name: 'I accept the EULA' }).click()
  await expect(page.getByText('Accept the Minecraft EULA to start test servers.')).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Start server', exact: true })).toHaveAttribute('aria-disabled', 'false')
})

test('the client only launches once you sign in to Minecraft', async ({ page }) => {
  await openApp(page, 'test', 'empty')
  await expect(page.getByLabel('Join server')).toBeDisabled()
  for (const name of ['Quick test', 'Launch client']) {
    await expect(page.getByRole('button', { name, exact: true })).toHaveAttribute('aria-disabled', 'true')
  }
  await page.getByRole('button', { name: 'Sign in with Microsoft' }).click()
  await expect(page.getByText('Finish signing in with Microsoft in your browser.')).toBeVisible()
  await expect(page.locator('#rail-player')).toHaveText('Minecraft: muonr')
  await expect(page.getByRole('button', { name: 'Quick test' })).toBeFocused()
  await expect(page.getByRole('button', { name: 'Launch client' })).toHaveAttribute('aria-disabled', 'false')
  await expect(page.getByText('Logged in as muonr for client runs.')).toBeVisible()
})

test('signing in beside Run unblocks the client there too', async ({ page }) => {
  await openApp(page, 'test', 'empty', 'settings')
  await page.locator('#minecraft-account').getByRole('button', { name: 'Sign in with Microsoft' }).click()
  await expect(page.getByRole('region', { name: 'Run, left pane' }).getByRole('button', { name: 'Launch client' })).toHaveAttribute('aria-disabled', 'false')
})

test('Join server stays as you left it when you come back to Run', async ({ page }) => {
  await openApp(page, 'test', 'ready')
  await page.getByLabel('Join server').check()
  const nav = page.getByRole('navigation', { name: 'Pack' })
  await nav.getByRole('button', { name: /^Mods/ }).click()
  await nav.getByRole('button', { name: /^Run/ }).click()
  await expect(page.getByLabel('Join server')).toBeChecked()
})

test('you can launch and kill the client from Mods', async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  const launch = page.getByRole('button', { name: /^Launch/ })
  await launch.click()
  // Kill appears beside Launch, which keeps focus while the client runs.
  await page.getByRole('button', { name: 'Kill client' }).waitFor()
  await expect(launch).toBeFocused()
  await expect(launch).toHaveAttribute('aria-disabled', 'true')
  await page.getByRole('button', { name: 'Kill client' }).click()
  await expect(launch).toHaveAttribute('aria-disabled', 'false')
})

test('you can start, restart and stop the test server from Server', async ({ page }) => {
  await openApp(page, 'server', 'ready')
  const start = page.getByRole('button', { name: 'Start server' })
  await start.click()
  const status = page.locator('.launch .status')
  await expect(status).toHaveText('The test server is up.')
  const restart = page.getByRole('button', { name: 'Restart server' })
  await expect(restart).toBeFocused()
  await expect(restart).not.toHaveClass(/primary/)

  // An override change only reaches the test server once it restarts, so we promote Restart.
  await page.getByLabel(/^Override for /).first().selectOption('keep')
  await expect(restart).toHaveClass(/primary/)
  await expect(status).toContainText('Restart it to use your changes.')
  await restart.click()
  await expect(page.getByRole('button', { name: 'Restarting' })).toBeFocused()
  await expect(restart).not.toHaveClass(/primary/)
  await expect(status).toHaveText('The test server is up.')

  await page.getByRole('button', { name: 'Stop server' }).click()
  await expect(start).toBeVisible()
  await expect(status).toHaveText('')
})

test('stopping during a restart cancels it', async ({ page }) => {
  await openApp(page, 'server', 'ready')
  await page.getByRole('button', { name: 'Start server' }).click()
  await page.getByRole('button', { name: 'Restart server' }).click()
  await page.getByRole('button', { name: 'Stop server' }).click()
  await expect(page.getByRole('button', { name: 'Start server' })).toBeVisible()
  await page.waitForTimeout(1000)
  await expect(page.getByRole('button', { name: 'Start server' })).toBeVisible()
})

test('Server and Run report a test server that stops on its own', async ({ page }) => {
  await openApp(page, 'server', 'ready')
  await page.getByRole('button', { name: 'Start server' }).click()
  await expect(page.locator('.launch .status')).toHaveText('The test server is up.')
  // Calling the backend directly skips the UI's stop, as a crash would.
  await page.evaluate(() => {
    const tauri = (globalThis as unknown as { __TAURI_INTERNALS__: { invoke: (command: string, args: unknown) => Promise<unknown> } }).__TAURI_INTERNALS__
    return tauri.invoke('kill_process', { which: 'server' })
  })
  await expect(page.locator('.launch .status')).toContainText('The test server stopped on its own')
  await page.getByRole('button', { name: 'See the log in Run' }).click()
  await expect(page.getByText(/The test server stopped on its own/)).toBeVisible()
})

test("starting a test clears the last run's logs", async ({ page }) => {
  await openApp(page, 'test', 'ready')
  await page.evaluate(() => {
    const emit = (globalThis as unknown as { __mockEmit: (event: string, payload: unknown) => void }).__mockEmit
    emit('job-log', { job: 'server', stream: 'info', text: 'Old server line' })
    emit('job-log', { job: 'client', stream: 'info', text: 'Old client line' })
  })
  await expect(page.getByText('Old server line')).toBeVisible()
  await expect(page.getByText('Old client line')).toBeVisible()
  await page.getByRole('button', { name: 'Quick test' }).click()
  await expect(page.getByText('Syncing changed files to the test server folder')).toBeVisible()
  await expect(page.getByText('Old server line')).toHaveCount(0)
  await expect(page.getByText('Old client line')).toHaveCount(0)
})

test('you can sign in to Minecraft and out again', async ({ page }) => {
  await openApp(page, 'settings', 'empty')
  const section = page.locator('#minecraft-account')
  await section.getByRole('button', { name: 'Sign in with Microsoft' }).click()
  await expect(section).toContainText('Finish signing in with Microsoft in your browser')
  await expect(section.getByRole('button', { name: 'Cancel' })).toBeFocused()
  await expect(section.getByRole('status')).toHaveText('Signed in as muonr.')
  await expect(page.locator('#rail-player')).toHaveText('Minecraft: muonr')
  await section.getByRole('button', { name: 'Sign out' }).click()
  await expect(section.getByRole('status')).toHaveText('Signed out of Minecraft.')
  await expect(section.getByRole('button', { name: 'Sign in with Microsoft' })).toBeFocused()
})

test('cancelling a Minecraft sign-in is not an error', async ({ page }) => {
  await openApp(page, 'settings', 'empty')
  const section = page.locator('#minecraft-account')
  await section.getByRole('button', { name: 'Sign in with Microsoft' }).click()
  await section.getByRole('button', { name: 'Cancel' }).click()
  await expect(section.getByRole('status')).toHaveText('Sign-in cancelled.')
  await expect(section.getByRole('alert')).toHaveCount(0)
})

test('server properties and startup save as you change them', async ({ page }) => {
  const problems: string[] = []
  page.on('console', (message) => message.type() === 'error' && !message.location().url.endsWith('/favicon.ico') && problems.push(message.text()))
  await openApp(page, 'server', 'dirty')
  await page.getByRole('tab', { name: 'Properties' }).click()
  const entry = page.getByRole('img', { name: /in a player's server list/ })
  await expect(entry).toContainText("Raven's High Fantasy 0.1.0")
  await expect(entry).toContainText('0/12')

  const motd = page.getByLabel('Message of the day')
  await motd.fill('Welcome ')
  await motd.press('End')
  await page.getByRole('button', { name: 'Green, §a' }).click()
  await expect(motd).toHaveValue('Welcome §a')
  await expect(page.getByRole('status').filter({ hasText: 'Saved' })).toBeVisible()

  await page.getByLabel('Difficulty', { exact: true }).selectOption('easy')
  await expect(page.getByRole('button', { name: /^Reset Difficulty/ })).toHaveCount(0)

  await page.getByRole('tab', { name: 'Startup' }).click()
  await page.getByPlaceholder('6G').fill('lots')
  await expect(page.getByText('Enter a size in gigabytes or megabytes')).toBeVisible()
  await page.getByRole('textbox', { name: 'JVM flags' }).fill('-Xmx4G')
  await expect(page.getByText('Set the heap size under Memory instead')).toBeVisible()
  await page.getByRole('textbox', { name: 'Server arguments' }).fill('--world\nMy World')
  await page.getByRole('textbox', { name: 'Server arguments' }).blur()
  await expect(page.getByRole('complementary', { name: 'What hosts run' })).toContainText('nogui --world "My World"')
  expect(problems).toEqual([])
})

test("installing a mod marks the dependencies it brings as installing", async ({ page }) => {
  await openApp(page, 'mods', 'ready')
  await page.getByRole('button', { name: 'Add from CurseForge' }).click()
  await page.getByRole('searchbox', { name: 'Search CurseForge' }).fill('create')
  await page.getByRole('button', { name: "Install Create: Steam 'n' Rails" }).click()
  await page.getByRole('button', { name: /^Install all/ }).click()
  await expect(page.getByRole('button', { name: /^(Cancel installing|Installing) Create$/ })).toContainText('Installing')
})

test('downloading GraalVM fills a Java version nobody has installed', async ({ page }) => {
  await openApp(page, 'settings', 'dirty')
  const slot = page.locator('.java', { has: page.locator('#java-25') })
  await expect(page.locator('#java-25 option:checked')).toHaveText('Automatic: no Java 25 found')
  await expect(slot).toContainText('None of your packs need it yet.')
  await slot.getByRole('button', { name: 'Download GraalVM 25' }).click()
  await expect(slot.getByRole('status')).toHaveText('Downloaded GraalVM 25.0.4. packraven now uses it for Java 25.')
  await expect(page.locator('#java-25 option:checked')).toHaveText('Automatic: GraalVM 25.0.4 · downloaded by packraven')
})

test('extra launch arguments check what Java will receive', async ({ page }) => {
  await openApp(page, 'pack', 'dirty')
  const flags = page.getByRole('textbox', { name: 'Client JVM flags' })
  await expect(flags).toBeHidden()
  await page.getByText('Extra launch arguments', { exact: true }).click()
  await flags.fill('--add-opens\njava.base/java.lang=ALL-UNNAMED')
  await expect(page.getByText("doesn't start with a dash")).toHaveCount(0)
  await flags.fill('-Dmixin.debug.export=true stray')
  await expect(page.getByText(`"stray" doesn't start with a dash`)).toBeVisible()
  await expect(page.getByText('1 problem')).toBeVisible()
  await flags.fill('-javaagent:"C:/Program Files/spark.jar\n# -XX:+UnlockDiagnosticVMOptions')
  await expect(page.getByText('opening double quote with no closing one')).toBeVisible()
  await flags.fill('-javaagent:"C:/Program Files/spark.jar"\n# -XX:+UnlockDiagnosticVMOptions')
  await expect(page.locator('code', { hasText: '-javaagent:C:/Program Files/spark.jar' })).toBeVisible()
  await page.getByRole('textbox', { name: 'Server arguments' }).fill('--world\nDebug World')
  await expect(page.getByText('(3 lines in use)')).toBeVisible()
  await expect(page.getByRole('status').filter({ hasText: /^Saved$/ })).toBeVisible()

  // Pack and app settings save as you edit them, without a Save button.
  await page.getByLabel('Author', { exact: true }).fill('Raven')
  await expect(page.getByRole('status').filter({ hasText: /^Saved$/ })).toBeVisible()

  // Saved arguments keep the fields open the next time you look.
  await page.getByRole('button', { name: 'Run', exact: true }).click()
  await page.getByRole('button', { name: 'Pack settings', exact: true }).click()
  await expect(page.getByRole('textbox', { name: 'Server arguments' })).toHaveValue('--world\nDebug World')
  await expect(page.getByLabel('Author', { exact: true })).toHaveValue('Raven')

  await page.getByRole('button', { name: 'App settings', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill('Raven Dev')
  await expect(page.getByRole('status').filter({ hasText: /^Saved$/ })).toBeVisible()
})

test('you can search the text of every file, open a match, and replace and undo', async ({ page }) => {
  await openApp(page, 'files', 'dirty')
  await page.keyboard.press('Control+Shift+F')
  const find = page.getByRole('textbox', { name: 'Search text in files' })
  await expect(find).toBeFocused()
  await find.fill('true')
  const summary = page.locator('#text-search-summary')
  await expect(summary).toHaveText('14 matches in 2 files')

  // Whole word and match case narrow the search, and their shortcuts work from the field.
  await page.keyboard.press('Alt+C')
  await expect(page.getByRole('button', { name: 'Match case' })).toHaveAttribute('aria-pressed', 'true')
  await page.keyboard.press('Alt+C')

  // The arrow keys move from the field into the results, and Enter opens the match in the editor.
  const results = page.getByRole('tree', { name: 'Search results' })
  await find.press('ArrowDown')
  await expect(results.getByRole('treeitem', { name: /^fml\.toml/ })).toBeFocused()
  await page.keyboard.press('ArrowDown')
  await page.keyboard.press('Enter')
  await expect(page.getByRole('tab', { name: 'fml.toml' })).toHaveAttribute('aria-selected', 'true')
  await expect(page.locator('.cm-activeLine')).toContainText('earlyWindowControl')

  // F4 steps to the next match from the editor.
  await page.keyboard.press('F4')
  await expect(page.locator('.cm-activeLine')).toContainText('disableOptimizedDFU')

  // Delete hides a match, so replacing leaves it alone.
  await results.getByRole('treeitem', { name: /^Line 12:/ }).click()
  await page.keyboard.press('Delete')
  await expect(summary).toContainText('13 matches in 2 files (1 hidden)')

  await page.keyboard.press('Control+Shift+H')
  const replaceWith = page.getByRole('textbox', { name: 'Replace with' })
  await expect(replaceWith).toBeFocused()
  await replaceWith.fill('false')
  await expect(results.getByRole('treeitem', { name: /^Line 4, replaces true with false:/ })).toBeVisible()

  // Replacing one match from the keyboard keeps your place in the list.
  await results.getByRole('treeitem', { name: /^Line 15,/ }).click()
  await page.keyboard.press('Shift+F10')
  await page.getByRole('menuitem', { name: 'Replace this match' }).click()
  await expect(page.getByRole('status').filter({ hasText: 'Replaced 1 match in shouldersurfing-client.toml.' })).toBeVisible()
  await expect(results.getByRole('treeitem', { name: /^Line 17,/ })).toBeFocused()

  page.once('dialog', (dialog) => {
    expect(dialog.message()).toContain('Replace 12 matches in 2 files with “false”?')
    dialog.accept()
  })
  await page.getByRole('button', { name: 'Replace 12 in 2 files' }).click()
  await expect(page.getByRole('status').filter({ hasText: 'Replaced 12 matches in 2 files.' })).toBeVisible()
  await expect(summary).toContainText('You hid the only match.')

  await page.getByRole('button', { name: 'Undo', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: 'Undid the replace.' })).toBeVisible()
  await expect(summary).toContainText('12 matches in 2 files (1 hidden)')

  // Esc goes back to the file tree, and the search is still there when you come back.
  await find.press('Escape')
  await expect(page.getByRole('heading', { name: 'Files', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Search text in files' }).click()
  await expect(page.getByRole('textbox', { name: 'Search text in files' })).toHaveValue('true')
})

test('an invalid regex says what is wrong', async ({ page }) => {
  await openApp(page, 'files', 'dirty')
  await page.keyboard.press('Control+Shift+F')
  await page.getByRole('button', { name: 'Use regular expression' }).click()
  await page.getByRole('textbox', { name: 'Search text in files' }).fill('(')
  await expect(page.locator('#text-search-summary')).toContainText("That isn't a valid regex")
})

test('selected files can be renamed together with a preview', async ({ page }) => {
  await openApp(page, 'files', 'dirty')
  const tree = page.getByRole('tree', { name: 'Pack files' })
  await tree.getByRole('treeitem', { name: 'datapacks', exact: true }).click()
  await tree.getByRole('treeitem', { name: 'AetherCompat1.0.zip' }).click()
  await tree.getByRole('treeitem', { name: 'QuarkCompat1.0.zip' }).click({ modifiers: ['Shift'] })
  await page.keyboard.press('F2')

  const dialog = page.getByRole('dialog', { name: 'Rename 5 items' })
  await expect(dialog.getByRole('textbox', { name: 'Find' })).toBeFocused()
  await dialog.getByRole('textbox', { name: 'Find' }).fill('Compat')
  await dialog.getByRole('textbox', { name: 'Replace with' }).fill('-compat')
  await expect(dialog.getByRole('row', { name: /AetherCompat1\.0\.zip Aether-compat1\.0\.zip/ })).toBeVisible()

  // Two items that would end up with one name block the rename until you change the settings.
  await dialog.getByRole('checkbox', { name: 'Regular expression' }).check()
  await dialog.getByRole('textbox', { name: 'Find' }).fill('^.*$')
  await dialog.getByRole('textbox', { name: 'Replace with' }).fill('same')
  await expect(dialog.getByRole('row', { name: /AetherCompat1\.0\.zip same\.zip 5 items would be named same\.zip/ })).toBeVisible()
  await expect(dialog.getByRole('status')).toContainText('5 new names have problems')
  await expect(dialog.getByRole('button', { name: 'Rename', exact: true })).toBeDisabled()

  // Skipping the items with problems leaves nothing to rename, and ticking them all brings them back.
  await dialog.getByRole('button', { name: 'Skip them' }).click()
  await expect(dialog.getByRole('status')).toContainText('Tick at least one item to rename.')
  await dialog.getByRole('checkbox', { name: 'Rename all' }).check()

  // Alt+3 switches to numbering and moves to its first field.
  await page.keyboard.press('Alt+3')
  await expect(dialog.getByRole('radio', { name: 'Number' })).toBeChecked()
  await dialog.getByRole('textbox', { name: 'New name' }).fill('compat')
  await dialog.getByRole('combobox', { name: 'Counter', exact: true }).selectOption('01')
  await dialog.getByRole('checkbox', { name: 'Rename CataclysmCompat1.0.zip' }).uncheck()
  await expect(dialog.getByRole('row', { name: /Irons_Spellbooks_Compat_1\.0\.0\.zip compat 02\.zip/ })).toBeVisible()
  await dialog.getByRole('button', { name: 'Rename 4 items' }).click()

  await expect(dialog).toBeHidden()
  await expect(page.getByRole('status').filter({ hasText: 'Renamed 4 items.' })).toBeAttached()
  for (const name of ['compat 01.zip', 'compat 04.zip', 'CataclysmCompat1.0.zip']) await expect(tree.getByRole('treeitem', { name })).toBeVisible()
  await expect(tree.getByRole('treeitem', { name: 'compat 01.zip' })).toHaveAttribute('aria-selected', 'true')
})

test('Format tidies the open file, keeps its comments, and undoes in one step', async ({ page }) => {
  await openApp(page, 'files', 'dirty')
  await page.getByRole('searchbox', { name: 'Search file and folder names' }).fill('packraven.toml')
  await page.keyboard.press('Enter')
  const editor = page.locator('.cm-content')
  const status = page.getByRole('status')
  await editor.click()
  await page.keyboard.press('Control+A')
  await page.keyboard.insertText('# pack\n[pack]\nname="x"   # note\nmods=[1,2,  3]\n\n\n\n[test]\nport=25565\n')
  await page.keyboard.press('Shift+Alt+F')
  await expect(status.filter({ hasText: "We formatted this file and checked that its values didn't change." })).toBeAttached()
  await expect(editor).toHaveText('# pack[pack]name = "x" # notemods = [1, 2, 3][test]port = 25565')
  await page.keyboard.press('Shift+Alt+F')
  await expect(status.filter({ hasText: 'This file is already formatted.' })).toBeAttached()

  // The button hands focus back to the editor, so undo works right after a click.
  await page.keyboard.press('Control+Z')
  await expect(editor).toContainText('name="x"   # note')
  await page.getByRole('button', { name: 'Format', exact: true }).click()
  await expect(editor).toContainText('name = "x" # note')
  await page.keyboard.press('Control+Z')
  await expect(editor).toContainText('name="x"   # note')

  // A file that doesn't parse stays as it is, and the note names the line to fix.
  await page.keyboard.press('Control+Home')
  await page.keyboard.insertText('[broken\n')
  await page.keyboard.press('Shift+Alt+F')
  await expect(status.filter({ hasText: 'Line 1 has a syntax error. Fix it, then format again.' })).toBeAttached()
  await expect(editor).toContainText('[broken')
})

test('Format tidies a KubeJS script with Prettier, and the linter flags its syntax errors', async ({ page }) => {
  await openApp(page, 'files', 'dirty')
  // The mock has no scripts, so we add one the way the New file action would.
  await page.evaluate(() => {
    const internals = (globalThis as unknown as { __TAURI_INTERNALS__: { invoke: (cmd: string, args: object) => Promise<unknown> } }).__TAURI_INTERNALS__
    return internals.invoke('create_file', { path: 'kubejs/server_scripts/recipes.js' })
  })
  await page.getByRole('searchbox', { name: 'Search file and folder names' }).fill('recipes.js')
  await page.keyboard.press('Enter')
  const editor = page.locator('.cm-content')
  await editor.click()
  await page.keyboard.press('Control+A')
  await page.keyboard.insertText("ServerEvents.recipes(event=>{\n  event.remove({output:'minecraft:stick'}) // no sticks\n})\n")
  await page.keyboard.press('Shift+Alt+F')
  await expect(page.getByRole('status').filter({ hasText: 'We formatted this file' })).toBeAttached()
  await expect(editor).toHaveText("ServerEvents.recipes(event => {  event.remove({ output: 'minecraft:stick' }) // no sticks})")
  await page.keyboard.press('Control+End')
  await page.keyboard.insertText('event.shaped(\n')
  await expect(page.locator('.cm-lint-marker-error')).toHaveCount(1)
})

test('a file in a mod jar can be overridden in a new data pack after adding Paxi', async ({ page }) => {
  await openApp(page, 'files', 'dirty')
  const tree = page.getByRole('tree', { name: 'Pack files' })
  const jar = 'mods/example-mod.jar'
  for (const path of ['mods', jar, `${jar}/data`, `${jar}/data/example`, `${jar}/data/example/recipe`, `${jar}/data/example/recipe/fire_charge.json`]) {
    await tree.locator(`[data-path="${path}"]`).click()
  }
  await page.getByRole('button', { name: 'Override in a data pack…' }).click()

  const dialog = page.getByRole('dialog', { name: 'Override in a data pack' })
  await expect(dialog.getByRole('button', { name: 'Add Paxi' })).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(dialog.getByText("Paxi requires YUNG's API, so we'll add it too.")).toBeVisible()
  await expect(dialog.getByRole('button', { name: 'Add 2 mods' })).toBeFocused()
  await page.keyboard.press('Enter')

  await expect(dialog.getByRole('textbox', { name: 'Name' })).toBeFocused()
  await dialog.getByRole('textbox', { name: 'Name' }).fill('Overrides')
  await dialog.getByRole('button', { name: 'Copy and open' }).click()

  await expect(dialog).toBeHidden()
  await expect(page.getByRole('status').filter({ hasText: 'Copied into Overrides.' })).toBeAttached()
  await expect(tree.locator('[data-path="config/paxi/datapacks/Overrides/data/example/recipe/fire_charge.json"]')).toHaveAttribute('aria-selected', 'true')
  await expect(page.getByRole('button', { name: /^Saved/ })).toBeVisible()

  await tree.locator(`[data-path="${jar}/data/example/recipe/fire_charge.json"]`).click()
  await tree.locator(`[data-path="${jar}/data/example/recipe/stone_bricks.json"]`).click({ modifiers: ['Shift'] })
  await tree.locator(`[data-path="${jar}/data/example/recipe/stone_bricks.json"]`).click({ button: 'right' })
  await page.getByRole('menuitem', { name: 'Override 2 items in a data pack…' }).click()

  await expect(dialog).toContainText('Has 1 of 2')
  await expect(dialog.getByRole('list', { name: 'Items to override' })).toContainText('data/example/recipe/stone_bricks.json')
  await dialog.getByRole('button', { name: 'Copy and show' }).click()

  await expect(dialog).toBeHidden()
  await expect(tree.locator('[data-path="config/paxi/datapacks/Overrides/data/example/recipe/fire_charge.json"]')).toHaveAttribute('aria-selected', 'true')
  await expect(tree.locator('[data-path="config/paxi/datapacks/Overrides/data/example/recipe/stone_bricks.json"]')).toHaveAttribute('aria-selected', 'true')
  await expect(tree).toContainText("Copied 2 files into Overrides. Minecraft uses these files instead of example-mod.jar's, along with 1 file the pack already had.")
})

test('one override choice can cover every version of a file', async ({ page }) => {
  await openApp(page, 'publish', 'dirty')
  const notice = page.getByRole('list', { name: 'Files not on Modrinth' })
  const result = page.locator('#modrinth-choice-result')
  await page.getByRole('button', { name: 'Show 3 more' }).click()

  // An update renamed a file you included, so the row offers to cover every version.
  const prominent = notice.getByRole('listitem').filter({ hasText: 'Prominent-GLOBAL-MC1.21.1-4.1.5.jar' })
  await expect(prominent).toContainText('You included another version before: Prominent-GLOBAL-MC1.21.1-4.1.4.jar')
  await prominent.getByRole('button', { name: /^Include every version/ }).click()
  await expect(result).toContainText('include every file matching mods/Prominent-GLOBAL-*.jar in the Modrinth pack, along with versions you add later')
  await expect(prominent).toHaveCount(0)

  // Undoing a choice that replaced an earlier version restores that earlier choice.
  await result.getByRole('button', { name: 'Undo' }).click()
  await expect(prominent).toContainText('You included another version before: Prominent-GLOBAL-MC1.21.1-4.1.4.jar')
  await prominent.getByRole('button', { name: /^Include every version/ }).click()

  // Unhosted files without a version number in their name still offer a choice for every version.
  const pack = notice.getByRole('listitem').filter({ hasText: 'resourcepacks/FreshAnimations-PT-BR.zip' })
  await pack.getByRole('button', { name: /^Include every version/ }).click()
  await expect(result).toContainText('include every file matching resourcepacks/FreshAnimations-PT-BR*.zip')

  // After a choice about one file, the result offers the same choice for its other versions.
  await notice.getByRole('listitem').filter({ hasText: 'AllTheLeaks' }).getByRole('button', { name: /^Include jar/ }).click()
  await notice.getByRole('button', { name: /^Leave out FTB Library/ }).click()
  await result.getByRole('button', { name: 'leave out every version matching mods/ftb-library-neoforge-*.jar' }).click()
  await expect(result).toContainText('leave every file matching mods/ftb-library-neoforge-*.jar out of')

  await result.getByRole('button', { name: 'Pack settings' }).click()
  const permissions = page.getByRole('region', { name: 'Modrinth permissions' })
  const openLicense = permissions.getByRole('list', { name: 'Included under an open license' })
  await openLicense.getByRole('listitem').filter({ hasText: 'AllTheLeaks' }).getByRole('button', { name: /^Include every version/ }).click()
  await expect(openLicense).toContainText('AllTheLeaks, every version')

  await permissions.locator('summary', { hasText: 'Included on your word (2 files)' }).click()
  const onYourWord = permissions.getByRole('list', { name: 'Included on your word' })
  await expect(onYourWord).toContainText('Matches Prominent-GLOBAL-MC1.21.1-4.1.5.jar now.')
  await expect(onYourWord).toContainText('Matches FreshAnimations-PT-BR.zip now.')
  await expect(permissions.getByRole('list', { name: 'Left out of the Modrinth pack' })).toContainText('FTB Library (NeoForge), every version')
})

test("the Modrinth pack can use Modrinth's own build of a version", async ({ page }) => {
  await openApp(page, 'publish', 'dirty')
  const derelict = page.getByRole('list', { name: 'Files not on Modrinth' }).getByRole('listitem').filter({ hasText: 'mods/derelict-2.1.0.jar' })
  const result = page.locator('#modrinth-choice-result')
  await expect(derelict).toContainText("Modrinth has its own build of 2.1.0, which differs from CurseForge's in 1 class, 1 image and 2 other files.")
  await expect(derelict).toContainText('We linked the two projects because they share file names.')
  // Moving both packs back changes the CurseForge pack too, so it never takes the primary action's place.
  await expect(derelict.getByRole('button', { name: /^Move both packs back to 2\.0\.0/ })).not.toHaveClass(/primary/)

  await derelict.getByRole('button', { name: /^Use Modrinth's 2\.1\.0/ }).click()
  await expect(result).toContainText("We'll use Modrinth's build of Mim1q's Derelict 2.1.0 in the Modrinth pack.")
  await expect(derelict).toHaveCount(0)
  await result.getByRole('button', { name: 'Undo' }).click()
  await expect(result).toContainText('Undone. Decide again below.')
  await expect(derelict).toHaveCount(1)
})

test('a link in one pane opens its target in the other', async ({ page }) => {
  await openApp(page, 'test', 'dirty', 'files')
  await page.getByRole('region', { name: 'Run, left pane' }).getByRole('button', { name: 'Manage your sign-in in App settings' }).click()
  await expect(page.getByRole('region', { name: 'Run, left pane' })).toBeVisible()
  await expect(page.getByRole('region', { name: 'App settings, right pane' })).toBeVisible()
})

test.describe('at 125% zoom of the minimum window', () => {
  test.use({ deviceScaleFactor: 1.25, viewport: { width: 768, height: 512 } })

  test('a split shows its panes as tabs', async ({ page }) => {
    await openApp(page, 'git', 'dirty', 'files')
    const tabs = page.getByRole('tablist', { name: 'Panes' })
    await expect(tabs.getByRole('tab', { name: 'Files' })).toHaveAttribute('aria-selected', 'true')
    await expect(page.getByRole('heading', { level: 2, name: 'Git', exact: true })).toBeHidden()
    await tabs.getByRole('tab', { name: 'Files' }).press('ArrowLeft')
    await expect(tabs.getByRole('tab', { name: 'Git' })).toBeFocused()
    await expect(page.getByRole('heading', { level: 2, name: 'Git', exact: true })).toBeVisible()
    await page.keyboard.press('F6')
    await expect(tabs.getByRole('tab', { name: 'Files' })).toHaveAttribute('aria-selected', 'true')
  })
})

for (const [left, right] of [['git', 'files'], ['mods', 'test'], ['changes', 'test'], ['server', 'test']] as [View, View][]) {
  test(`${left} and ${right} render side by side`, async ({ page }) => {
    const problems: string[] = []
    page.on('pageerror', (error) => problems.push(`uncaught: ${error.message}`))
    page.on('console', (message) => message.type() === 'error' && !message.location().url.endsWith('/favicon.ico') && problems.push(message.text()))
    await openApp(page, left, 'dirty', right)
    await page.waitForTimeout(500)
    expect(problems).toEqual([])
  })
}

test("a pack we can't load still opens in Files and Git, so you can roll it back", async ({ page }) => {
  await openApp(page, 'git', 'newerlock', 'files')
  const newer = /A newer version of packraven wrote/
  for (const name of ['Git, left pane', 'Files, right pane']) {
    await expect(page.getByRole('region', { name }).getByRole('alert').filter({ hasText: newer })).toBeVisible()
  }

  await page.getByRole('navigation', { name: 'Pack' }).getByRole('button', { name: /^Mods/ }).click()
  const mods = page.getByRole('region', { name: 'Mods, right pane' })
  await expect(mods.getByRole('alert').filter({ hasText: newer })).toBeVisible()
  await expect(mods.getByRole('heading', { level: 2, name: 'Mods', exact: true })).toHaveCount(0)
})

test('views open side by side and keep their shortcuts to the focused pane', async ({ page }) => {
  await openApp(page, 'mods', 'dirty')
  const rail = page.getByRole('navigation', { name: 'Pack' })
  await rail.getByRole('button', { name: /^Git/ }).click({ modifiers: ['ControlOrMeta'] })
  const left = page.getByRole('region', { name: 'Mods, left pane' })
  const right = page.getByRole('region', { name: 'Git, right pane' })
  await expect(left).toBeVisible()
  await expect(right).toBeVisible()
  await expect(rail.getByRole('button', { name: /^Git/ })).toHaveAttribute('aria-current', 'page')

  // Ctrl+F finds in Mods only while its pane has focus.
  await right.getByRole('heading', { name: 'Git', exact: true }).click()
  await page.keyboard.press('ControlOrMeta+f')
  await expect(left.getByRole('searchbox', { name: 'Find a mod or author' })).not.toBeFocused()
  await left.getByRole('heading', { name: 'Mods', exact: true }).click()
  await page.keyboard.press('ControlOrMeta+f')
  await expect(left.getByRole('searchbox', { name: 'Find a mod or author' })).toBeFocused()

  // A rail click replaces the focused pane's view, and a view already on screen just takes focus.
  await rail.getByRole('button', { name: /^Changes/ }).click()
  await expect(page.getByRole('region', { name: 'Changes, left pane' })).toBeVisible()
  await rail.getByRole('button', { name: /^Git/ }).click()
  await expect(page.getByRole('region', { name: 'Changes, left pane' })).toBeVisible()

  await page.locator('.bar', { hasText: 'Git' }).click({ button: 'right' })
  await page.getByRole('menuitem', { name: 'Swap sides' }).click()
  await expect(page.getByRole('region', { name: 'Git, left pane' })).toBeVisible()
  // The close button leaves with its pane, so focus moves to the pane that stays.
  await page.getByRole('button', { name: 'Close Changes' }).click()
  await expect(page.getByRole('region', { name: /pane$/ })).toHaveCount(0)
  await expect(page.getByRole('heading', { level: 2, name: 'Git', exact: true })).toBeVisible()
  await expect(page.locator('#pane-0')).toBeFocused()

  await page.keyboard.press('ControlOrMeta+Backslash')
  await expect(page.getByRole('region', { name: /pane$/ })).toHaveCount(2)
  await page.keyboard.press('ControlOrMeta+Backslash')
  await expect(page.getByRole('region', { name: /pane$/ })).toHaveCount(0)

  // Dragging a rail item onto the left or right half of the work area opens it on that side,
  // and Escape cancels a drag without switching views.
  const panes = await page.locator('.panes').boundingBox()
  const filesBtn = await rail.getByRole('button', { name: /^Files/ }).boundingBox()
  await page.mouse.move(filesBtn!.x + filesBtn!.width / 2, filesBtn!.y + filesBtn!.height / 2)
  await page.mouse.down()
  await page.mouse.move(panes!.x + panes!.width * 0.25, panes!.y + panes!.height / 2)
  await expect(page.locator('.pane-drop.left')).toHaveText('Left pane')
  await page.keyboard.press('Escape')
  await page.mouse.up()
  await expect(page.getByRole('region', { name: /pane$/ })).toHaveCount(0)
  await expect(page.getByRole('heading', { level: 2, name: 'Git', exact: true })).toBeVisible()

  await page.mouse.move(filesBtn!.x + filesBtn!.width / 2, filesBtn!.y + filesBtn!.height / 2)
  await page.mouse.down()
  await page.mouse.move(panes!.x + panes!.width * 0.25, panes!.y + panes!.height / 2)
  await page.mouse.up()
  await expect(page.getByRole('region', { name: 'Files, left pane' })).toBeVisible()
  await expect(page.getByRole('region', { name: 'Git, right pane' })).toBeVisible()
})

test('a rail context menu stays open while a live log scrolls', async ({ page }) => {
  await openApp(page, 'test', 'ready')
  await page.evaluate(() => {
    const emit = (globalThis as unknown as { __mockEmit: (event: string, payload: unknown) => void }).__mockEmit
    for (let i = 1; i <= 40; i++) emit('job-log', { job: 'server', stream: 'stdout', text: `[12:00:00] [Server thread/INFO] [minecraft/DedicatedServer]: Line ${i}` })
  })
  const rail = page.getByRole('navigation', { name: 'Pack' })
  await rail.getByRole('button', { name: /^Mods/ }).click({ button: 'right' })
  const menu = page.getByRole('menu', { name: 'Pane actions' })
  await expect(menu).toBeVisible()

  await page.evaluate(() => {
    const emit = (globalThis as unknown as { __mockEmit: (event: string, payload: unknown) => void }).__mockEmit
    for (let i = 41; i <= 80; i++) emit('job-log', { job: 'server', stream: 'stdout', text: `[12:00:01] [Server thread/INFO] [minecraft/DedicatedServer]: Line ${i}` })
  })
  await expect(page.getByText('Line 80')).toBeVisible()
  await expect(menu).toBeVisible()
  await menu.getByRole('menuitem', { name: 'Open beside' }).click()
  await expect(page.getByRole('region', { name: 'Mods, right pane' })).toBeVisible()
})

test('tracked files your .gitignore covers can be untracked, or kept with an exception', async ({ page }) => {
  await openApp(page, 'git', 'dirty')
  await page.getByRole('button', { name: 'Review…' }).click()
  const dialog = page.getByRole('dialog', { name: 'Stop tracking ignored files' })
  const all = dialog.getByRole('checkbox', { name: 'All 4 files' })
  await expect(all).toBeChecked()
  // We open on a checkbox, so Enter right away doesn't untrack everything.
  await expect(all).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(dialog).toBeVisible()

  await dialog.getByRole('checkbox', { name: 'config/jei/jei-colors.ini.bak.1' }).uncheck()
  await expect(dialog.getByRole('checkbox', { name: /Keep the unchecked file tracked/ })).toBeChecked()
  await dialog.getByRole('button', { name: 'Stop tracking 3 files' }).click()
  await expect(dialog).toBeHidden()
  const done = page.getByRole('status').filter({ hasText: 'Stopped tracking 3 files.' })
  await expect(done).toBeVisible()
  await expect(done.getByRole('button', { name: 'Undo' })).toBeVisible()
  await expect(page.getByText('your .gitignore leaves out.')).toBeHidden()
})

test("packraven's changelog opens in a modal and concatenates skipped versions", async ({ page }) => {
  await openApp(page, 'settings', 'dirty')
  const updates = page.locator('#updates')
  await updates.getByRole('button', { name: "What's new" }).click()

  const availableDialog = page.getByRole('dialog', { name: "What's new in packraven 0.2.0" })
  await expect(availableDialog).toBeVisible()
  await expect(availableDialog).toContainText('You have 0.1.0 (2 releases behind).')
  await expect(availableDialog.getByRole('heading', { level: 3, name: '0.2.0' })).toBeVisible()
  await expect(availableDialog.getByRole('heading', { level: 3, name: '0.1.9' })).toBeVisible()
  await expect(availableDialog).toContainText('Sign in to Minecraft right from the Run view')
  await expect(availableDialog).toContainText('Reduced number of permission prompt popups on macOS')

  // Restarting to update finishes into 0.2.0 and pops up the post-update changelog window.
  await availableDialog.getByRole('button', { name: 'Restart now' }).click()
  const updatedDialog = page.getByRole('dialog', { name: 'Updated to packraven 0.2.0' })
  await expect(updatedDialog).toBeVisible()
  await expect(updatedDialog).toContainText('Updated from 0.1.0 across 2 releases.')
  await expect(updatedDialog.getByRole('heading', { level: 3, name: '0.2.0' })).toBeVisible()
  await expect(updatedDialog.getByRole('heading', { level: 3, name: '0.1.9' })).toBeVisible()
  await updatedDialog.getByRole('button', { name: 'Close' }).click()
  await expect(updatedDialog).toBeHidden()

  // While up to date, What's new still reopens the release notes.
  await updates.getByRole('button', { name: "What's new" }).click()
  await expect(page.getByRole('dialog', { name: "What's new in packraven 0.2.0" })).toBeVisible()
})

test("a failed publish in one pack stays with that pack after you switch", async ({ page }) => {
  await openApp(page, 'publish', 'ready')
  await page.evaluate(() => {
    const emit = (globalThis as unknown as { __mockEmit: (event: string, payload: unknown) => void }).__mockEmit
    emit('job-state', { job: 'publish', state: 'running', message: null })
    emit('job-state', { job: 'publish', state: 'failed', message: 'CurseForge rejected the upload (500 Internal Server Error)' })
  })
  const failure = page.getByText('CurseForge rejected the upload (500 Internal Server Error)')
  await expect(failure).toBeVisible()

  const rail = page.getByRole('navigation', { name: 'Pack' })
  const switchTo = async (name: string) => {
    await page.getByRole('button', { name: /Raven's High Fantasy|Otherworld/ }).first().click()
    await page.getByRole('menuitemradio', { name: new RegExp(name) }).click()
    await expect(page.getByRole('heading', { level: 1, name })).toBeVisible()
    await rail.getByRole('button', { name: /^Publish/ }).click()
    await expect(page.getByRole('heading', { level: 2, name: 'Publish', exact: true })).toBeVisible()
  }
  await switchTo('Otherworld')
  await expect(failure).toHaveCount(0)
  await switchTo("Raven's High Fantasy")
  await expect(failure).toBeVisible()
})

test('a server pack that failed to upload can be uploaded again from Publish', async ({ page }) => {
  page.on('dialog', (dialog) => dialog.accept())
  await page.goto('/?view=publish&scenario=ready&serverpackfails')
  const publish = page.locator('.go .primary')
  await expect(publish).toBeEnabled({ timeout: 10000 })
  await publish.click()
  const notice = page.locator('.notice', { hasText: "but its server pack isn't" })
  await expect(notice).toBeVisible()
  await expect(notice.getByRole('button', { name: 'Show server pack' })).toBeVisible()
  await expect(notice.getByRole('button', { name: "Open CurseForge's upload page" })).toBeVisible()

  await notice.getByRole('button', { name: 'Upload it again' }).click()
  await expect(page.getByText(/is on CurseForge and Modrinth\./)).toBeVisible()
  await expect(page.getByText("but its server pack isn't")).toHaveCount(0)
})

test('a release packraven left unfinished can be finished from Publish', async ({ page }) => {
  const dialogs: string[] = []
  page.on('dialog', (dialog) => {
    dialogs.push(dialog.message())
    dialog.accept()
  })
  await openApp(page, 'publish', 'dirty')
  const notice = page.locator('.notice', { has: page.getByRole('heading', { name: /we didn't finish publishing it/ }) })
  const left = notice.getByRole('list', { name: /^Left to do for/ })
  await expect(left.getByRole('listitem')).toHaveCount(3)
  await expect(left).toContainText('Upload the server pack to CurseForge, after building it again')
  await expect(notice).toContainText('Your mods changed since')
  // The manual route stays folded away until you ask for it.
  await expect(notice.getByRole('button', { name: "Open CurseForge's upload page" })).toBeHidden()

  await notice.getByRole('button', { name: /^Finish publishing/ }).click()
  await expect(page.getByText(/^Finished publishing/)).toBeVisible()
  await expect(notice).toHaveCount(0)
  expect(dialogs[0]).toContain('Build the server pack again from the pack as it is now')
  expect(dialogs[0]).toContain("won't match what's already on CurseForge")
})

test('a server pack you uploaded yourself stops being offered for upload', async ({ page }) => {
  await openApp(page, 'publish', 'dirty')
  const notice = page.locator('.notice', { has: page.getByRole('heading', { name: /we didn't finish publishing it/ }) })
  await notice.getByText('Upload the server pack yourself').click()
  await notice.getByRole('button', { name: 'I uploaded it' }).click()
  await expect(notice.getByRole('list', { name: /^Left to do for/ })).not.toContainText('server pack')
  await expect(notice.getByRole('heading', { name: /we didn't finish publishing it/ })).toBeFocused()
})
