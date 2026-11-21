import { expect, test } from '@playwright/test'
import { openApp } from './app.ts'

test('installed mods show gallery tab, direct links, ctrl-click open, and maven context menu items', async ({ page }) => {
  await openApp(page, 'mods', 'dirty')

  const firstRow = page.locator('tbody tr.row').first()
  await expect(firstRow).toBeVisible()

  // Holding Control highlights links for immediate feedback; Ctrl+click opens the page instead of expanding.
  await page.keyboard.down('Control')
  await expect(page.locator('.table')).toHaveClass(/ctrl-open/)
  await firstRow.click({ modifiers: ['Control'] })
  await page.keyboard.up('Control')
  await expect(page.locator('tr.details')).toHaveCount(0)

  // Normal click expands and loads external links + gallery tab.
  await firstRow.click()
  const details = page.locator('tr.details')
  await expect(details).toBeVisible()
  await expect(details.getByRole('button', { name: 'Discord' })).toBeVisible()
  await expect(details.getByRole('button', { name: 'Source' })).toBeVisible()
  await expect(details.getByRole('button', { name: 'Wiki' })).toBeVisible()
  await expect(details.getByRole('button', { name: 'Issues' })).toBeVisible()

  const galleryTab = details.getByRole('button', { name: /^Gallery/ })
  await expect(galleryTab).toContainText('3')
  await galleryTab.click()
  const shots = details.locator('.gallery .shot')
  await expect(shots).toHaveCount(3)

  // Clicking a gallery image opens the lightbox, moves focus in, steps with arrows, and returns focus on Escape.
  const firstShot = shots.first()
  await firstShot.click()
  const lightbox = page.getByRole('dialog')
  await expect(lightbox).toBeVisible()
  await expect(lightbox.getByRole('button', { name: 'Close' })).toBeFocused()
  await expect(lightbox).toContainText('1 / 3')
  await page.keyboard.press('ArrowRight')
  await expect(lightbox).toContainText('2 / 3')
  await page.keyboard.press('Escape')
  await expect(lightbox).toHaveCount(0)
  await expect(firstShot).toBeFocused()

  // Right-click context menu on a mod row has gallery, CurseMaven snippet, file ID, and project ID.
  await firstRow.click({ button: 'right' })
  const menu = page.getByRole('menu')
  await expect(menu.getByRole('menuitem', { name: 'Show gallery' })).toBeVisible()
  await expect(menu.getByRole('menuitem', { name: 'Copy CurseMaven snippet' })).toBeEnabled()
  await expect(menu.getByRole('menuitem', { name: 'Copy file ID' })).toBeEnabled()
  await expect(menu.getByRole('menuitem', { name: 'Copy project ID' })).toBeEnabled()
  await page.keyboard.press('Escape')
})

test('CurseForge and Modrinth browse support gallery, links, ctrl-click, and context menus', async ({ page }) => {
  await openApp(page, 'mods', 'dirty')

  // CurseForge browse
  await page.getByRole('button', { name: 'Add from CurseForge' }).click()
  const cfItem = page.locator('.results ul li').first()
  await expect(cfItem).toBeVisible()

  await cfItem.click({ modifiers: ['Control'] })
  await expect(page.locator('.reading')).toHaveCount(0)

  await cfItem.click({ button: 'right' })
  const cfMenu = page.getByRole('menu')
  await expect(cfMenu.getByRole('menuitem', { name: 'Show gallery' })).toBeVisible()
  await expect(cfMenu.getByRole('menuitem', { name: 'Copy CurseMaven snippet' })).toBeEnabled()
  await expect(cfMenu.getByRole('menuitem', { name: 'Copy file ID' })).toBeEnabled()
  await page.keyboard.press('Escape')

  await cfItem.click()
  const cfReading = page.locator('.reading')
  await expect(cfReading).toBeVisible()
  await expect(cfReading.getByRole('button', { name: 'Discord' })).toBeVisible()
  await cfReading.getByRole('button', { name: /^Gallery/ }).click()
  await expect(cfReading.locator('.gallery .shot')).toHaveCount(3)

  // Pressing Escape inside the browse gallery lightbox must close only the lightbox, not ModBrowser.
  await cfReading.locator('.gallery .shot').first().click()
  await expect(page.getByRole('dialog')).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await expect(page.getByRole('heading', { name: 'Add from CurseForge' })).toBeVisible()

  // Switch to Modrinth browse
  await page.getByRole('button', { name: /Back to installed/ }).click()
  await page.getByRole('button', { name: 'Add from Modrinth' }).click()
  const mrItem = page.locator('.results ul li').first()
  await expect(mrItem).toBeVisible()

  await mrItem.click({ modifiers: ['Control'] })
  await expect(page.locator('.reading')).toHaveCount(0)

  await mrItem.click({ button: 'right' })
  const mrMenu = page.getByRole('menu')
  await expect(mrMenu.getByRole('menuitem', { name: 'Show gallery' })).toBeVisible()
  await expect(mrMenu.getByRole('menuitem', { name: 'Copy Modrinth Maven snippet' })).toBeEnabled()
  await expect(mrMenu.getByRole('menuitem', { name: 'Copy version ID' })).toBeEnabled()
  await expect(mrMenu.getByRole('menuitem', { name: 'Copy project ID' })).toBeEnabled()
  await page.keyboard.press('Escape')

  await mrItem.click()
  const mrReading = page.locator('.reading')
  await expect(mrReading).toBeVisible()
  await expect(mrReading.getByRole('button', { name: 'Discord' })).toBeVisible()
  await expect(mrReading.getByRole('button', { name: 'Source' })).toBeVisible()
  await mrReading.getByRole('button', { name: /^Gallery/ }).click()
  await expect(mrReading.locator('.gallery .shot')).toHaveCount(3)
})

test('returning to Mods renders cached rows immediately and virtualizes large packs', async ({ page }) => {
  await page.goto('/?view=mods&scenario=ready&manymods')
  const region = page.getByRole('region', { name: 'Installed mods' })
  await expect(region.locator('tbody > tr.row').first()).toBeVisible()

  // With 450+ mods, only a windowed slice of rows is mounted in the DOM.
  const mountedCount = await region.locator('tbody > tr.row').count()
  expect(mountedCount).toBeGreaterThan(10)
  expect(mountedCount).toBeLessThan(120)

  // Scrolling down brings later rows into the rendered window.
  await region.evaluate((el) => {
    el.scrollTop = el.scrollHeight
  })
  await expect(region.locator('tbody > tr.row').last()).toContainText('Synthetic Mod 450')

  // Switching away and back renders the list immediately without skeleton rows.
  const nav = page.getByRole('navigation', { name: 'Pack' })
  await nav.getByRole('button', { name: /^Git/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Git' })).toBeVisible()
  await nav.getByRole('button', { name: /^Mods/ }).click()
  await expect(region.locator('tbody > tr.skeleton')).toHaveCount(0)
  await expect(region.locator('tbody > tr.row').first()).toBeVisible()

  // Filtering a large list resets scroll and finds matches anywhere in the pack.
  await page.getByRole('searchbox', { name: 'Find a mod or author' }).fill('Synthetic Mod 450')
  await expect(region.locator('tbody > tr.row')).toHaveCount(1)
  await expect(region.locator('tbody > tr.row').first()).toContainText('Synthetic Mod 450')
})

test('returning to Server renders cached rows immediately and virtualizes large mod lists', async ({ page }) => {
  await page.goto('/?view=server&scenario=ready&manymods')
  const region = page.getByRole('region', { name: 'Mods' })
  await expect(region.locator('tbody > tr[data-path]').first()).toBeVisible()

  await page.getByRole('group', { name: 'Show' }).getByRole('button', { name: 'All mods' }).click()
  const mountedCount = await region.locator('tbody > tr[data-path]').count()
  expect(mountedCount).toBeGreaterThan(10)
  expect(mountedCount).toBeLessThan(120)

  // Switching away and back renders Server immediately without skeleton rows.
  const nav = page.getByRole('navigation', { name: 'Pack' })
  await nav.getByRole('button', { name: /^Mods/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Mods' })).toBeVisible()
  await nav.getByRole('button', { name: /^Server/ }).click()
  await expect(region.locator('tbody > tr.skeleton')).toHaveCount(0)
  await expect(region.locator('tbody > tr[data-path]').first()).toBeVisible()
})

test('returning to Git, Changes, and Files renders cached views immediately', async ({ page }) => {
  await openApp(page, 'git', 'dirty')
  const nav = page.getByRole('navigation', { name: 'Pack' })

  // Select a changed file so its diff is loaded and cached.
  const firstChange = page.locator('.changes .row').first()
  await expect(firstChange).toBeVisible()
  await firstChange.click()
  await expect(page.locator('.diff')).toBeVisible()

  // Visit Changes, confirm mod changes appear, and type unsaved notes.
  await nav.getByRole('button', { name: /^Changes/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Changes' })).toBeVisible()
  await expect(page.locator('.change-group').first()).toBeVisible()
  const notes = page.locator('#release-notes')
  await notes.fill('Draft note kept across navigation')

  // Visit Files and expand a folder.
  await nav.getByRole('button', { name: /^Files/ }).click()
  const tree = page.getByRole('tree')
  await expect(tree.getByRole('treeitem').first()).toBeVisible()
  const folder = tree.getByRole('treeitem', { expanded: false }).first()
  const folderName = (await folder.textContent())?.trim() ?? ''
  await folder.click()
  await expect(tree.getByRole('treeitem', { expanded: true }).first()).toBeVisible()

  // Return to Git: list and commits stay populated without loading state, and reselecting the file shows cached diff.
  await nav.getByRole('button', { name: /^Git/ }).click()
  await expect(page.locator('.changes .row').first()).toBeVisible()
  await page.locator('.changes .row').first().click()
  await expect(page.locator('.diff')).toBeVisible()

  // Return to Changes: cached mod diff and unsaved notes remain visible immediately.
  await nav.getByRole('button', { name: /^Changes/ }).click()
  await expect(page.locator('.change-group').first()).toBeVisible()
  await expect(page.locator('#release-notes')).toHaveValue('Draft note kept across navigation')

  // Return to Files: previously expanded folder stays expanded immediately.
  await nav.getByRole('button', { name: /^Files/ }).click()
  await expect(tree.getByRole('treeitem', { expanded: true }).first()).toBeVisible()
  if (folderName) await expect(tree.getByRole('treeitem', { expanded: true }).first()).toContainText(folderName)
})
