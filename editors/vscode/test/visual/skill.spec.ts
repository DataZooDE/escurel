import { expect, test } from '@playwright/test';

test('skill page renders in the current theme', async ({ page }, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.goto(`/test/visual/harness/skill.html?theme=${theme}`);
  await page.locator('escurel-skill-page h1').waitFor();
  await expect(page).toHaveScreenshot('skill-page.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});
