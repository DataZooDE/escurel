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
  await page.goto(`/test/visual/harness/thread.html?theme=${theme}&scenario=branches&view=first`);
  await page.locator('escurel-thread-canvas').waitFor();
  await expect(page).toHaveScreenshot('thread-lanes.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});

// The same thread zoomed out to the overview (Fit): below 85% every card keeps its icon, accent bar,
// type word, title, state chip and the Needs-you icon, and drops its body.
test('thread canvas zoomed out shows the low-zoom form in the current theme', async ({
  page,
}, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.goto(`/test/visual/harness/thread.html?theme=${theme}&scenario=branches&view=fit`);
  await page.locator('escurel-thread-canvas').waitFor();
  await expect(page).toHaveScreenshot('thread-overview.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});

// The lowest zoom: words are counter-scaled, so the titles stay legible at 40%.
test('thread canvas at 40% keeps readable words in the current theme', async ({
  page,
}, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.goto(`/test/visual/harness/thread.html?theme=${theme}&scenario=branches&zoom=0.4`);
  await page.locator('escurel-thread-canvas').waitFor();
  await expect(page).toHaveScreenshot('thread-overview-40.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});

// Nothing selected: the details view says what to do.
test('details view empty state in the current theme', async ({ page }, testInfo) => {
  const theme = (testInfo.project.metadata as { theme: string }).theme;
  await page.goto(`/test/visual/harness/details.html?theme=${theme}`);
  await page.locator('escurel-details').waitFor();
  await expect(page).toHaveScreenshot('details-empty.png', {
    maxDiffPixelRatio: 0.01,
    fullPage: true,
  });
});

// The details view (the inspector, now in the bottom panel) has to look right in every theme: the Skill split buttons on an
// instance, and the control bar on a run, including the control that is deactivated for a non-admin.
for (const select of ['run', 'draft']) {
  test(`thread inspector with actions: ${select}`, async ({ page }, testInfo) => {
    const theme = (testInfo.project.metadata as { theme: string }).theme;
    await page.goto(`/test/visual/harness/details.html?theme=${theme}&select=${select}`);
    await page.locator('escurel-thread-inspector').waitFor();
    await expect(page).toHaveScreenshot(`thread-inspector-${select}.png`, {
      maxDiffPixelRatio: 0.01,
      fullPage: true,
    });
  });
}
