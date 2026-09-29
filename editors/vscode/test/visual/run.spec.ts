import { expect, test } from '@playwright/test';

test('run detail renders in the current theme', async ({ page }, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.goto(`/test/visual/harness/run.html?theme=${theme}`);
  await page.locator('escurel-run-detail').waitFor();
  await expect(page).toHaveScreenshot('run-detail.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});
