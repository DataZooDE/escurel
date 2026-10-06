import { expect, test } from '@playwright/test';

for (const state of ['morning', 'quiet'] as const) {
  test(`overview board (${state}) renders in the current theme`, async ({ page }, testInfo) => {
    const theme = (testInfo.project.metadata as { theme: string }).theme;
    await page.setViewportSize({ width: 1100, height: 760 });
    await page.goto(
      `/test/visual/harness/overview.html?theme=${theme}&state=${state === 'quiet' ? 'quiet' : ''}`,
    );
    await page.locator('escurel-overview section.tile').first().waitFor();
    await expect(page).toHaveScreenshot(`overview-${state}.png`, {
      maxDiffPixelRatio: 0.01,
      fullPage: true,
    });
  });
}
