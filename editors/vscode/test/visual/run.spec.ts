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

// The control bar is part of the run header and has to look right in every theme, including its
// deactivated button (a control that is not yours still shows, with the reason on hover).
for (const state of ['running', 'planned', 'dead_letter']) {
  test(`run detail with controls: ${state}`, async ({ page }, testInfo) => {
    const theme = (testInfo.project.metadata as { theme: string }).theme;
    await page.goto(`/test/visual/harness/run.html?theme=${theme}&state=${state}`);
    await page.locator('escurel-run-detail .run-control').first().waitFor();
    await expect(page).toHaveScreenshot(`run-detail-${state}.png`, {
      maxDiffPixelRatio: 0.01,
      fullPage: true,
    });
  });
}

// The trace is the point of run detail for an engineer: tool, outcome in words, duration, bar, and the
// link to what the run produced. A failed call is marked with a word and a cross, not colour alone.
test('run detail trace', async ({ page }, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.goto(`/test/visual/harness/run.html?theme=${theme}&state=trace`);
  await page.locator('escurel-run-detail .tool-call').first().waitFor();
  await expect(page).toHaveScreenshot('run-detail-trace.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});
