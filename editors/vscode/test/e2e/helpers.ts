import type { Page } from '@playwright/test';
import { expect } from './fixtures';

export const pane = (page: Page, title: string) =>
  page.locator('.pane', { has: page.locator('.pane-header', { hasText: title }) });

/**
 * The tree views only render the rows in view (the list is virtualised), and the Knowledge tree now
 * holds folders, so a row may not exist until it is scrolled to. Scroll from the top, a step at a time,
 * until the row is rendered; no test depends on how tall the window happens to be.
 */
export async function scanForRow(page: Page, name: RegExp) {
  const k = pane(page, 'Knowledge');
  const row = k.getByRole('treeitem', { name });
  const list = k.locator('.monaco-list').first();
  const first = list.locator('.monaco-list-row[data-index="0"]');
  // To the top: the list's own keyboard handling (Home focuses the first row and scrolls it into view)
  // and the wheel together. A row's tooltip, left over from an earlier hover, can swallow either, so a
  // miss is retried from a clean pointer instead of waiting on a list that will not move.
  for (let attempt = 0; attempt < 5 && (await first.count()) === 0; attempt += 1) {
    await page.mouse.move(1000, 700);
    await page.keyboard.press('Escape');
    await list.hover();
    await list.focus();
    await page.keyboard.press('Home');
    await page.mouse.wheel(0, -10_000);
    await expect(first)
      .toHaveCount(1, { timeout: 3_000 })
      .catch(() => undefined);
  }
  // At the top when the first row (index 0) is rendered: a condition, not a sleep.
  await expect(first).toHaveCount(1);
  await list.hover();
  const rendered = () =>
    list.evaluate((el) =>
      Array.from(el.querySelectorAll('.monaco-list-row'))
        .map((r) => r.getAttribute('data-index'))
        .join(','),
    );
  for (let i = 0; i < 40 && (await row.count()) === 0; i += 1) {
    const before = await rendered();
    await page.mouse.wheel(0, 120);
    // Scrolled when the set of rendered rows changed; at the bottom it never does (hence the cap).
    await expect
      .poll(rendered, { timeout: 1_500 })
      .not.toBe(before)
      .catch(() => undefined);
  }
  return (await row.count()) > 0 ? row.first() : undefined;
}

export async function knowledgeRow(page: Page, name: RegExp) {
  // A tooltip left by the last hover would cover the rows below it.
  await page.mouse.move(1000, 700);
  const row =
    (await scanForRow(page, name)) ??
    pane(page, 'Knowledge').getByRole('treeitem', { name }).first();
  await expect(row).toBeVisible();
  return row;
}

/** A skill row by what a screen reader hears: its role, then its id. */
export const skillRow = (page: Page, id: string) =>
  knowledgeRow(page, new RegExp(`skill (?:\\(inferred\\) )?${id},`));

/**
 * Open one row of a skill. The tree refreshes whenever something live happens (the runner is still
 * settling right after the window opens), and a refresh collapses what was just expanded, so the skill
 * is re-expanded and the row looked for again rather than trusting a single click.
 */
export async function openRow(page: Page, skill: string, row: RegExp) {
  for (let attempt = 0; attempt < 6; attempt += 1) {
    const skillItem = await skillRow(page, skill);
    // A live refresh can leave a stale row over this one for a moment (the click is then refused as
    // intercepted): a short wait and another attempt, not a 30 s timeout that fails the scenario.
    if ((await skillItem.getAttribute('aria-expanded')) !== 'true')
      await skillItem.click({ timeout: 5_000 }).catch(() => undefined);
    await page.mouse.move(1000, 700);
    // The children are fetched from the outside system: wait for the row, not for a duration.
    const found = await expect
      .poll(async () => (await scanForRow(page, row)) !== undefined, { timeout: 10_000 })
      .toBe(true)
      .then(() => true)
      .catch(() => false);
    const item = found ? await scanForRow(page, row) : undefined;
    if (item) {
      await item.click();
      return;
    }
  }
  throw new Error(`the row ${row} never appeared under ${skill}`);
}

/**
 * Choose an item of the context menu that is open: hover it (the menu highlights what is under the
 * pointer) and press Enter. A click can land while a live refresh is re-rendering the menu, and then
 * highlights the item without running it.
 */
export async function chooseMenuItem(page: Page, name: string) {
  const item = page.locator('.monaco-menu .action-item', { hasText: name }).first();
  await item.hover();
  await expect(item).toHaveClass(/focused/);
  await page.keyboard.press('Enter');
}

/**
 * Opens the command palette and filters it to `filter`, waiting until `listed` is shown. A palette typed
 * into the moment it opens can drop the text: the real windows of a loaded CI machine have shown the
 * UNFILTERED command list for 30 s after `fill('>...')` (anofox-evolve PR #27, native Evolve job; the
 * holdout journey in evolve-service.spec.ts had already needed a click and a value check for the same
 * reason). So: type, confirm the box holds what was typed AND the list shows the command, and otherwise
 * close the palette and start over (a few times) instead of waiting on a list that will not change.
 */
export async function filterCommandPalette(page: Page, filter: string, listed: string) {
  const widget = page.locator('.quick-input-widget');
  const input = widget.locator('.quick-input-box input');
  const list = page.locator('.quick-input-list');
  let last: unknown;
  for (let attempt = 0; attempt < 4; attempt += 1) {
    await page.keyboard.press('Control+Shift+P');
    await expect(widget).toBeVisible();
    await input.click();
    await input.fill(`>${filter}`);
    try {
      await expect(input).toHaveValue(`>${filter}`, { timeout: 5_000 });
      await expect(list).toContainText(listed, { timeout: 10_000 });
      return;
    } catch (error) {
      last = error;
      await page.keyboard.press('Escape');
      await expect(widget)
        .toBeHidden({ timeout: 5_000 })
        .catch(() => undefined);
    }
  }
  throw last;
}
