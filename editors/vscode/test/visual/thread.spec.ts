import { expect, test } from '@playwright/test';

test('thread canvas renders in the current theme', async ({ page }, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.goto(`/test/visual/harness/thread.html?theme=${theme}`);
  await page.locator('escurel-thread-canvas').waitFor();
  await expect(page).toHaveScreenshot('thread-canvas.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});
