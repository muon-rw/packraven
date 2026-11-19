import { expect, test, type Page } from '@playwright/test'
import { openApp } from './app.ts'

async function pressMouseNav(page: Page, button: 3 | 4) {
  await page.evaluate(`(() => {
    const target = document.activeElement instanceof Element ? document.activeElement : document.body
    for (const type of ['mousedown', 'mouseup', 'auxclick']) {
      target.dispatchEvent(new MouseEvent(type, { button: ${button}, bubbles: true, cancelable: true }))
    }
  })()`)
}

test('Mods unwinds expanded rows, selection, and search filter with Escape and MB4', async ({ page }) => {
  await openApp(page, 'mods', 'dirty')

  // Visit Files then Mods so there is a page in history behind Mods.
  await page.getByRole('button', { name: /^Files/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Files', exact: true })).toBeVisible()
  await page.getByRole('button', { name: /^Mods/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Mods', exact: true })).toBeVisible()

  const search = page.getByRole('searchbox', { name: 'Find a mod or author' })
  await search.fill('a')
  const firstRow = page.locator('tbody tr.row').first()
  await expect(firstRow).toBeVisible()

  // Select a checkbox and expand the row, moving focus out of the search box.
  await firstRow.locator('input[type="checkbox"]').check()
  await firstRow.click()
  await expect(page.locator('tr.details')).toHaveCount(1)
  await expect(page.locator('.bulk')).toBeVisible()

  // 1st Escape: collapses expanded row.
  await page.keyboard.press('Escape')
  await expect(page.locator('tr.details')).toHaveCount(0)
  await expect(page.locator('.bulk')).toBeVisible()
  await expect(search).toHaveValue('a')

  // 2nd Escape: clears selection.
  await page.keyboard.press('Escape')
  await expect(page.locator('.bulk')).toHaveCount(0)
  await expect(search).toHaveValue('a')

  // 3rd Escape: clears search filter even though focus is not in the search box.
  await page.keyboard.press('Escape')
  await expect(search).toHaveValue('')

  // 4th Escape: stays on Mods (Escape never leaves the page).
  await page.keyboard.press('Escape')
  await expect(page.getByRole('heading', { level: 2, name: 'Mods', exact: true })).toBeVisible()

  // Now verify MB4 clears search filter first, then navigates back to Files.
  await search.fill('a')
  await firstRow.click()
  await expect(page.locator('tr.details')).toHaveCount(1)

  await pressMouseNav(page, 3)
  await expect(page.locator('tr.details')).toHaveCount(0)
  await expect(search).toHaveValue('a')

  await pressMouseNav(page, 3)
  await expect(search).toHaveValue('')
  await expect(page.getByRole('heading', { level: 2, name: 'Mods', exact: true })).toBeVisible()

  await pressMouseNav(page, 3)
  await expect(page.getByRole('heading', { level: 2, name: 'Files', exact: true })).toBeVisible()

  // MB5 goes forward back to Mods.
  await pressMouseNav(page, 4)
  await expect(page.getByRole('heading', { level: 2, name: 'Mods', exact: true })).toBeVisible()
})

test('ModBrowser unwinds detail, search text, browse mode, and then page history', async ({ page }) => {
  await openApp(page, 'mods', 'dirty')

  await page.getByRole('button', { name: /^Git/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Git', exact: true })).toBeVisible()
  await page.getByRole('button', { name: /^Mods/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Mods', exact: true })).toBeVisible()

  await page.getByRole('button', { name: 'Add from CurseForge' }).click()
  const browseSearch = page.getByRole('searchbox', { name: 'Search CurseForge' })
  await expect(browseSearch).toBeVisible()

  const firstResult = page.locator('.results ul li').first()
  await expect(firstResult).toBeVisible()
  await browseSearch.fill('create')
  await firstResult.click()
  await expect(page.locator('.reading')).toHaveCount(1)

  // MB4 #1 collapses result detail.
  await pressMouseNav(page, 3)
  await expect(page.locator('.reading')).toHaveCount(0)
  await expect(browseSearch).toHaveValue('create')

  // MB4 #2 clears browse search text.
  await pressMouseNav(page, 3)
  await expect(browseSearch).toHaveValue('')
  await expect(browseSearch).toBeVisible()

  // MB4 #3 exits browse mode back to Installed mods.
  await pressMouseNav(page, 3)
  await expect(browseSearch).toHaveCount(0)
  await expect(page.getByRole('heading', { level: 2, name: 'Mods', exact: true })).toBeVisible()

  // MB4 #4 goes back to Git.
  await pressMouseNav(page, 3)
  await expect(page.getByRole('heading', { level: 2, name: 'Git', exact: true })).toBeVisible()
})

test('page history tracks views and Server tabs across back and forward navigation', async ({ page }) => {
  await openApp(page, 'mods', 'dirty')

  await page.getByRole('button', { name: /^Server/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Server', exact: true })).toBeVisible()

  const serverSearch = page.getByRole('searchbox', { name: 'Find a mod' })
  await serverSearch.fill('a')
  await page.getByRole('heading', { level: 2, name: 'Server', exact: true }).click()
  // MB4 clears Server search filter before navigating back.
  await pressMouseNav(page, 3)
  await expect(serverSearch).toHaveValue('')
  await expect(page.getByRole('heading', { level: 2, name: 'Server', exact: true })).toBeVisible()

  await page.getByRole('tab', { name: 'Properties' }).click()
  await expect(page.getByRole('tab', { name: 'Properties' })).toHaveAttribute('aria-selected', 'true')

  await page.getByRole('button', { name: /^Git/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Git', exact: true })).toBeVisible()

  // Alt+Left: Git -> Server (Properties) -> Server (Mods) -> Mods
  await page.keyboard.press('Alt+ArrowLeft')
  await expect(page.getByRole('heading', { level: 2, name: 'Server', exact: true })).toBeVisible()
  await expect(page.getByRole('tab', { name: 'Properties' })).toHaveAttribute('aria-selected', 'true')

  await page.keyboard.press('Alt+ArrowLeft')
  await expect(page.getByRole('tab', { name: 'Mods' })).toHaveAttribute('aria-selected', 'true')

  await page.keyboard.press('Alt+ArrowLeft')
  await expect(page.getByRole('heading', { level: 2, name: 'Mods', exact: true })).toBeVisible()

  // Alt+Right: Mods -> Server (Mods) -> Server (Properties) -> Git
  await page.keyboard.press('Alt+ArrowRight')
  await expect(page.getByRole('heading', { level: 2, name: 'Server', exact: true })).toBeVisible()
  await expect(page.getByRole('tab', { name: 'Mods' })).toHaveAttribute('aria-selected', 'true')

  await page.keyboard.press('Alt+ArrowRight')
  await expect(page.getByRole('tab', { name: 'Properties' })).toHaveAttribute('aria-selected', 'true')

  // Navigating to Changes from the middle truncates forward history (Git).
  await page.getByRole('button', { name: /^Changes/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Changes', exact: true })).toBeVisible()
  await page.keyboard.press('Alt+ArrowRight')
  await expect(page.getByRole('heading', { level: 2, name: 'Changes', exact: true })).toBeVisible()
})

test('MB4 closes a context menu without dismissing the view behind it', async ({ page }) => {
  await openApp(page, 'mods', 'dirty')

  const firstRow = page.locator('tbody tr.row').first()
  await firstRow.click()
  await expect(page.locator('tr.details')).toHaveCount(1)

  await firstRow.click({ button: 'right' })
  await expect(page.getByRole('menu')).toBeVisible()

  // MB4 closes only the menu; the expanded row remains open.
  await pressMouseNav(page, 3)
  await expect(page.getByRole('menu')).toHaveCount(0)
  await expect(page.locator('tr.details')).toHaveCount(1)
})

test('rail Back and Forward buttons reflect history and Escape restores focus on collapse', async ({ page }) => {
  await openApp(page, 'mods', 'dirty')

  const backBtn = page.getByRole('button', { name: 'Back', exact: true })
  const fwdBtn = page.getByRole('button', { name: 'Forward', exact: true })
  await expect(backBtn).toBeDisabled()
  await expect(fwdBtn).toBeDisabled()

  await page.getByRole('button', { name: /^Files/ }).click()
  await expect(page.getByRole('heading', { level: 2, name: 'Files', exact: true })).toBeVisible()
  await expect(backBtn).toBeEnabled()
  await expect(fwdBtn).toBeDisabled()

  await backBtn.click()
  await expect(page.getByRole('heading', { level: 2, name: 'Mods', exact: true })).toBeVisible()
  await expect(backBtn).toBeDisabled()
  await expect(fwdBtn).toBeEnabled()

  await fwdBtn.click()
  await expect(page.getByRole('heading', { level: 2, name: 'Files', exact: true })).toBeVisible()
  await backBtn.click()

  // Expanding a mod row, focusing inside its details, and pressing Escape restores focus to the row toggle.
  const firstRow = page.locator('tbody tr.row').first()
  const rowToggle = firstRow.locator('button.expand')
  await firstRow.click()
  const details = page.locator('tr.details')
  await expect(details).toBeVisible()
  await details.getByRole('button', { name: 'Description' }).focus()
  await page.keyboard.press('Escape')
  await expect(details).toHaveCount(0)
  await expect(rowToggle).toBeFocused()
})
