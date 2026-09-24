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
