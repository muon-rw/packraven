import { expect, test } from '@playwright/test'
import { openApp } from './app.ts'

test('datapack JSON files show Schemas toolbar reload, lint errors, completion and hover', async ({ page }) => {
  await openApp(page, 'files', 'dirty')

  // Open a read-only recipe inside example-mod.jar first.
  const tree = page.getByRole('tree', { name: 'Pack files' })
  const jar = 'mods/example-mod.jar'
  for (const path of ['mods', jar, `${jar}/data`, `${jar}/data/example`, `${jar}/data/example/recipe`, `${jar}/data/example/recipe/fire_charge.json`]) {
    await tree.locator(`[data-path="${path}"]`).click()
  }

  // Read-only jar copy should not show the editable Schemas toolbar control.
  await expect(page.getByRole('button', { name: 'Schemas', exact: true })).toHaveCount(0)

  // Override into an editable datapack and open it.
  await page.getByRole('button', { name: 'Override in a data pack…' }).click()
  const dialog = page.getByRole('dialog', { name: 'Override in a data pack' })
  await expect(dialog.getByRole('button', { name: 'Add Paxi' })).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(dialog.getByRole('button', { name: 'Add 2 mods' })).toBeFocused()
  await page.keyboard.press('Enter')
  await expect(dialog.getByRole('textbox', { name: 'Name' })).toBeFocused()
  await dialog.getByRole('textbox', { name: 'Name' }).fill('Overrides')
  await dialog.getByRole('button', { name: 'Copy and open' }).click()
  await expect(dialog).toBeHidden()

  // Schemas button appears and reloads cleanly.
  const schemasBtn = page.getByRole('button', { name: 'Schemas', exact: true })
  await expect(schemasBtn).toBeVisible()
  await schemasBtn.click()
  await expect(page.locator('.toolbar-note', { hasText: /Reloaded datapack schemas/ })).toBeVisible()

  // Dump registries runs the headless ProbeRaven dump and upgrades the source.
  const dumpBtn = page.getByRole('button', { name: 'Dump registries', exact: true })
  await expect(dumpBtn).toBeVisible()
  await dumpBtn.click()

  // The dump log opens with the current stage, elapsed time, and the server's output.
  const dumpPanel = page.getByRole('region', { name: 'Registry dump' })
  await expect(dumpPanel).toBeVisible()
  await expect(dumpPanel.getByRole('status')).toContainText('Launching headless server', { timeout: 5000 })
  await expect(dumpPanel).toContainText('Found 312 mods', { timeout: 5000 })
  await expect(dumpPanel.locator('.elapsed')).toHaveText(/^0:0\d$/)

  // Escape hides the log while the dump keeps running, and the toolbar button brings it back.
  await page.keyboard.press('Escape')
  await expect(dumpPanel).toBeHidden()
  const dumpingBtn = page.getByRole('button', { name: 'Dumping…', exact: true })
  await expect(dumpingBtn).toHaveAttribute('aria-expanded', 'false')
  await dumpingBtn.click()
  await expect(dumpPanel).toBeVisible()

  // The handle under the log resizes it, and Enter puts it back.
  const dumpHandle = page.getByRole('separator', { name: 'Resize the registry dump log' })
  const startHeight = (await dumpPanel.boundingBox())!.height
  await dumpHandle.focus()
  await page.keyboard.press('Shift+ArrowDown')
  expect((await dumpPanel.boundingBox())!.height).toBeGreaterThan(startHeight)
  await page.keyboard.press('Enter')
  await expect.poll(async () => (await dumpPanel.boundingBox())!.height).toBe(startHeight)

  // Stop ends the run quietly instead of reporting a failure.
  await dumpPanel.getByRole('button', { name: 'Stop' }).click()
  await expect(dumpPanel.getByRole('status')).toHaveText(/^Stopped the registry dump\./)
  await expect(dumpPanel.locator('.summary.failed')).toHaveCount(0)
  await expect(dumpPanel.getByRole('button', { name: 'Dump again' })).toBeVisible()
  await expect(dumpBtn).toBeVisible()
  await expect(schemasBtn).not.toHaveAttribute('title', /ProbeRaven dump/)

  // A full run upgrades the source and leaves the summary in the panel.
  await dumpBtn.click()
  await expect(dumpPanel.getByRole('status')).toContainText('Dumped 117 registries', { timeout: 10000 })
  await expect(schemasBtn).toHaveAttribute('title', /ProbeRaven dump/)
  await dumpPanel.getByRole('button', { name: 'Close' }).click()
  await expect(dumpPanel).toBeHidden()

  // The last run's log stays reachable, and closing the panel hands focus back to its toggle.
  const dumpLogBtn = page.getByRole('button', { name: 'Dump log', exact: true })
  await expect(dumpLogBtn).toBeFocused()
  await dumpLogBtn.click()
  await expect(dumpPanel.getByRole('status')).toContainText('Dumped 117 registries')
  await dumpPanel.getByRole('button', { name: 'Close' }).click()

  // Edit the recipe to trigger a datapack lint error ("count": 0).
  const content = page.locator('.cm-content')
  await content.click()
  await page.keyboard.press('Control+A')
  await page.keyboard.insertText('{\n  "type": "minecraft:crafting_shapeless",\n  "result": {\n    "id": "create:",\n    "count": 0\n  }\n}\n')

  // Async datapack linter surfaces the problem in the lint gutter and toolbar.
  await expect(page.locator('.cm-lint-marker-error')).toHaveCount(1, { timeout: 5000 })

  // Trigger autocomplete automatically by moving into the "create:" string value (no Ctrl+Space needed).
  const idLine = page.locator('.cm-line', { hasText: '"create:"' })
  await idLine.click()
  await page.keyboard.press('End')
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.press('ArrowLeft')
  await expect(page.locator('.cm-tooltip-autocomplete')).toContainText('create:brass_ingot')

  // Escape dismisses the popup and stays dismissed while arrowing inside that same string.
  await page.keyboard.press('Escape')
  await expect(page.locator('.cm-tooltip-autocomplete')).toHaveCount(0)
  await page.keyboard.press('ArrowLeft')
  await page.waitForTimeout(150)
  await expect(page.locator('.cm-tooltip-autocomplete')).toHaveCount(0)

  // Selecting inside the string and pressing Backspace (delete.selection) reopens autocomplete immediately.
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Backspace')
  await expect(page.locator('.cm-tooltip-autocomplete')).toContainText('create:brass_ingot')

  // Accepting replaces the string with a full valid ID; clicking at its end suppresses the lone self-match.
  await page.keyboard.press('Enter')
  await expect(page.locator('.cm-tooltip-autocomplete')).toHaveCount(0)
  await page.keyboard.press('ArrowDown')
  await page.keyboard.press('ArrowUp')
  await page.keyboard.press('End')
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.press('ArrowLeft')
  await page.waitForTimeout(150)
  await expect(page.locator('.cm-tooltip-autocomplete')).toHaveCount(0)

  // Selecting the ID suffix inside the full string opens completions filtered by the prefix before selection.
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await page.keyboard.press('Shift+ArrowLeft')
  await expect(page.locator('.cm-tooltip-autocomplete')).toContainText('create:brass_ingot')
  await page.keyboard.press('Escape')

  // Hover on "result" shows schema hover documentation.
  const resultToken = page.locator('.cm-line', { hasText: '"result"' }).getByText('"result"')
  await resultToken.hover()
  await expect(page.locator('.cm-probejs-tip')).toContainText('Output item stack produced by the recipe.')
})
