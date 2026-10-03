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

// The thread that has work waiting on a person and a second cascade branch (its own lane row).
test('thread canvas with waiting work and lanes renders in the current theme', async ({
  page,
}, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.goto(`/test/visual/harness/thread.html?theme=${theme}&scenario=branches`);
  await page.locator('escurel-thread-canvas').waitFor();
  await expect(page).toHaveScreenshot('thread-lanes.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});

// The inspector's actions have to look right in every theme: the Skill split buttons on an
// instance, and the control bar on a run, including the control that is deactivated for a non-admin.
for (const select of ['run', 'draft']) {
  test(`thread inspector with actions: ${select}`, async ({ page }, testInfo) => {
    const theme = (testInfo.project.metadata as { theme: string }).theme;
    await page.goto(`/test/visual/harness/thread.html?theme=${theme}&select=${select}`);
    await page.locator('escurel-thread-inspector').waitFor();
    await expect(page).toHaveScreenshot(`thread-inspector-${select}.png`, {
      maxDiffPixelRatio: 0.01,
      fullPage: true,
    });
  });
}
