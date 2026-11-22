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
  await expect(page.locator('.toolbar-note', { hasText: /Dumped 117 registries/ })).toBeVisible({ timeout: 5000 })
  await expect(schemasBtn).toHaveAttribute('title', /ProbeRaven dump/)

  // Edit the recipe to trigger a datapack lint error ("count": 0).
  const content = page.locator('.cm-content')
  await content.click()
  await page.keyboard.press('Control+A')
  await page.keyboard.insertText('{\n  "type": "minecraft:crafting_shapeless",\n  "result": {\n    "id": "create:",\n    "count": 0\n  }\n}\n')

  // Async datapack linter surfaces the problem in the lint gutter and toolbar.
  await expect(page.locator('.cm-lint-marker-error')).toHaveCount(1, { timeout: 5000 })

  // Trigger autocomplete inside the "create:" string value.
  const idLine = page.locator('.cm-line', { hasText: '"create:"' })
  await idLine.click()
  await page.keyboard.press('End')
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.press('ArrowLeft')
  await page.keyboard.press('Control+Space')
  await expect(page.locator('.cm-tooltip-autocomplete')).toContainText('create:brass_ingot')
  await page.keyboard.press('Escape')

  // Hover on "result" shows schema hover documentation.
  const resultToken = page.locator('.cm-line', { hasText: '"result"' }).getByText('"result"')
  await resultToken.hover()
  await expect(page.locator('.cm-probejs-tip')).toContainText('Output item stack produced by the recipe.')
})
