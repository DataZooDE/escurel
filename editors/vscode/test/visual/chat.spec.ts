import { expect, test } from '@playwright/test';

// The Chat view as it sits in the secondary sidebar (narrow), in each theme kind.
test('chat view renders in the current theme', async ({ page }, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.setViewportSize({ width: 360, height: 900 });
  await page.goto(`/test/visual/harness/chat.html?theme=${theme}`);
  await page.locator('escurel-chat').waitFor();
  await expect(page).toHaveScreenshot('chat.png', { maxDiffPixelRatio: 0.01 });
});
