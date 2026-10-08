import type { Page } from '@playwright/test';
import { expect, test, webviewWith } from './fixtures';

// The calm window, the way the demo opens by default: focus mode on, the overview board as the first
// screen. Played in order in ONE window; each scenario asserts what a person would SEE and leaves a
// screenshot in artifacts/ for a human to look at.
test.use({ focus: true });
test.describe.configure({ mode: 'serial' });

/** The colour the editor area is painting now: the theme in use. */
const editorBackground = (page: Page) =>
  page.evaluate(() => getComputedStyle(document.querySelector('.part.editor')!).backgroundColor);
const CALM_BACKGROUND = 'rgb(251, 250, 248)';

/** The titles of the icons in the activity bar (it sits at the top of the sidebar in the calm window). */
const activityIcons = (page: Page) =>
  page
    .locator(
      '.part.activitybar a.action-label[aria-label], .part.sidebar .composite-bar a.action-label[aria-label]',
    )
    .evaluateAll((els) =>
      els.map((e) => (e.getAttribute('aria-label') ?? '').replace(/\s*\(.*$/, '')),
    );

test('the window opens calm: no menu bar, command center or status bar, and the overview is the first screen', async ({
  stack,
}) => {
  const { page } = stack;
  const board = await webviewWith(page, 'escurel-overview section.tile');
  // The five answers, in the order a day starts.
  await expect(board.locator('section.tile h2')).toHaveText([
    'Decisions waiting',
    'Agent activity',
    'Needs attention',
    'Open items',
    'Recently finished',
  ]);
  await expect(board.locator('section.tile .headline').first()).toContainText('waiting for you');

  // The developer chrome is gone (each of these was seen present in the classic window).
  await expect(page.locator('.part.statusbar')).toBeHidden();
  await expect(page.locator('.menubar')).toBeHidden();
  await expect(page.locator('.command-center')).toHaveCount(0);
  // "folder - Visual Studio Code" is gone. (A window started with --extensionDevelopmentPath, as the demo
  // is, always prefixes "[Extension Development Host]"; an installed extension does not.)
  await expect(page.locator('.window-title')).toHaveText(/(^|\] )Escurel$/);

  // The stock developer icons are not offered; Escurel's own is.
  const icons = await activityIcons(page);
  expect(icons.join(' | ')).toMatch(/Escurel/);
  for (const stock of ['Explorer', 'Search', 'Source Control', 'Run and Debug', 'Extensions']) {
    expect(icons, `the ${stock} icon is hidden`).not.toContain(stock);
  }
  // The Escurel Calm theme is the one painting.
  await expect.poll(() => editorBackground(page)).toBe(CALM_BACKGROUND);
  await stack.shot('20-focus-overview');
});

test('a line on the board opens the thing it names', async ({ stack }) => {
  const { page } = stack;
  const board = await webviewWith(page, 'escurel-overview section.tile');
  // Decisions waiting: the first line is a set of proposed changes; opening it asks what to do with it.
  await board.locator('section.tile').first().locator('button.item').first().click();
  const picker = page.locator('.quick-input-widget');
  await expect(picker).toBeVisible();
  await expect(picker.getByRole('option', { name: /Apply all changes/ })).toBeVisible();
  await stack.shot('21-focus-board-opened-a-decision');
  await page.keyboard.press('Escape');
  await expect(picker).toBeHidden();
});

test('the board leaves the focus view, and brings it back, without losing the person’s window', async ({
  stack,
}) => {
  const { page } = stack;
  const board = await webviewWith(page, 'escurel-overview button.focus-toggle');
  await board.locator('button.focus-toggle').filter({ hasText: 'Leave focus view' }).click();

  // The classic chrome returns.
  await expect(page.locator('.part.statusbar')).toBeVisible();
  await expect(page.locator('.menubar')).toBeVisible();
  await expect(page.locator('.command-center')).toBeVisible();
  await expect(page.locator('.window-title')).not.toHaveText(/(^|\] )Escurel$/);
  // (The demo profile keeps the stock activity-bar icons hidden: that is the profile's own state, which
  // leaving the focus view does not touch.)
  expect((await activityIcons(page)).join(' | ')).toMatch(/Escurel/);
  // ... and the person's own theme is back, not Calm.
  await expect.poll(() => editorBackground(page)).not.toBe(CALM_BACKGROUND);
  await stack.shot('22-focus-off');

  // And the same button brings it back.
  const again = await webviewWith(page, 'escurel-overview button.focus-toggle');
  await again.locator('button.focus-toggle').filter({ hasText: 'Switch to focus view' }).click();
  await expect(page.locator('.part.statusbar')).toBeHidden();
  await expect.poll(() => editorBackground(page)).toBe(CALM_BACKGROUND);
  await stack.shot('23-focus-on-again');
});
