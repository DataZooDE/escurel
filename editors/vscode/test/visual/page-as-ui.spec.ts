import { expect, test } from '@playwright/test';

// One screenshot per theme kind; the harness injects only --vscode-* tokens,
// so a component that hard-codes a colour shows up as the same pixels in all
// three (and fails the lint before it gets here).
test('page-as-ui renders in the current theme', async ({ page }, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.goto(`/test/visual/harness/index.html?theme=${theme}`);
  await page.locator('escurel-page-as-ui').waitFor();
  await expect(page).toHaveScreenshot('page-as-ui.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});

// What the source system holds beneath the form, for each kind of backend, in each theme.
for (const kind of ['rows', 'fields', 'document', 'issue']) {
  test(`page-as-ui previews a ${kind} source in the current theme`, async ({ page }, testInfo) => {
    const theme = (testInfo.project.metadata as { theme: string }).theme;
    await page.goto(`/test/visual/harness/index.html?theme=${theme}&preview=${kind}`);
    await page.locator('escurel-source-preview').waitFor();
    await expect(page).toHaveScreenshot(`page-as-ui-preview-${kind}.png`, {
      maxDiffPixelRatio: 0.01,
      fullPage: true,
    });
  });
}

// A row of an `instances: rows` skill: the strip that says it is a read-only source row with its own
// notes, and the quiet accent on the columns that belong to the source.
test('a row instance says it is read-only source data with its own notes', async ({
  page,
}, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.goto(`/test/visual/harness/index.html?theme=${theme}&variant=row`);
  await page.locator('escurel-page-as-ui .source-strip').waitFor();
  await expect(page).toHaveScreenshot('page-as-ui-row.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});

// A row from an outside REST / MCP system: marked as external data, a way to propose a change, and what
// the last change did (applied; or: the source is unreachable and the change did not go through).
for (const variant of ['external', 'external-down']) {
  test(`a ${variant} row in the current theme`, async ({ page }, testInfo) => {
    const theme = (testInfo.project.metadata as { theme: string }).theme;
    await page.goto(`/test/visual/harness/index.html?theme=${theme}&variant=${variant}`);
    await page.locator('escurel-page-as-ui .source-strip').waitFor();
    await expect(page).toHaveScreenshot(`page-as-ui-${variant}.png`, {
      maxDiffPixelRatio: 0.01,
      fullPage: true,
    });
  });
}
