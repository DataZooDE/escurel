import type { Page } from '@playwright/test';
import { expect, test, webviewWith } from './fixtures';

// One window for the whole file, played in order: each scenario leaves the stack as the next one can
// use it. Every one asserts what a person would SEE and leaves a screenshot in artifacts/ for a human
// to look at, because "the assertion passed" says nothing about whether it looks right.
test.describe.configure({ mode: 'serial' });

const pane = (page: Page, title: string) =>
  page.locator('.pane', { has: page.locator('.pane-header', { hasText: title }) });

test('the story is on screen: knowledge, threads, awaiting, inbox and the runner', async ({
  stack,
}) => {
  const { page } = stack;
  await expect(
    pane(page, 'Knowledge').getByRole('treeitem', { name: /customer-order review/ }),
  ).toBeVisible();
  await expect(pane(page, 'Awaiting You').getByRole('treeitem').first()).toBeVisible();
  await expect(pane(page, 'Inbox').getByRole('treeitem').first()).toBeVisible();
  await expect(pane(page, 'Runner').getByRole('treeitem', { name: /Health ok/ })).toBeVisible();
  await expect(pane(page, 'Runner').getByRole('treeitem', { name: /harness: echo/ })).toBeVisible();
  await stack.shot('01-overview');
});

test('the thread shows the cascade, and an instance offers a skill to start', async ({ stack }) => {
  const { page } = stack;
  const wv = await webviewWith(page, 'escurel-thread-canvas');
  const canvas = wv.locator('escurel-thread-canvas');
  await expect(canvas.getByRole('treeitem')).toHaveCount(6);
  await stack.shot('02-thread');

  // The instance a draft proposes a change to: select it, and the inspector offers its skill's actions.
  await canvas.getByRole('treeitem', { name: /order-4500123/ }).click();
  const start = wv.getByRole('button', { name: /with an agent/ }).first();
  await expect(start).toBeVisible();
  await stack.shot('03-thread-inspector-instance');

  // Click it. A start event appears in the Inbox, and the runner takes it.
  const inbox = pane(page, 'Inbox').getByRole('treeitem', {
    name: /supplier-risk · order-4500123/,
  });
  const before = await inbox.count();
  await start.click();
  await expect.poll(() => inbox.count()).toBeGreaterThan(before);
  await stack.shot('04-after-start');
});

test('run detail opens from the canvas with its plan and tool calls', async ({ stack }) => {
  const { page } = stack;
  // Scenario 2 started a skill, which opened that event's thread in a NEW tab. Go back to the story's
  // thread, the one with a finished run, as a person would.
  await page.locator('.tab', { hasText: 'PO 4500087412' }).first().click();
  const wv = await webviewWith(page, 'escurel-thread-canvas');
  // The canvas is panned to whatever was selected last; Fit brings every card back into view.
  await wv.getByRole('button', { name: 'Fit' }).click();
  // By its visible text: a card's accessible name is its title, subtitle and state, not its meta lines.
  await wv
    .locator('escurel-thread-canvas')
    .getByRole('treeitem')
    .filter({ hasText: 'echo · review' })
    .first()
    .dblclick();
  await expect(page.locator('.tab .label-name', { hasText: /^Run / })).toBeVisible();
  const run = await webviewWith(page, 'escurel-run-detail');
  await expect(run.locator('escurel-run-detail h1')).toContainText('Run ');
  await expect(run.getByText('Tool calls')).toBeVisible();
  await stack.shot('05-run-detail');
});

test('a sales order opens as a real order page: SAP fields and an items table', async ({
  stack,
}) => {
  const { page } = stack;
  const knowledge = pane(page, 'Knowledge');
  await knowledge.getByRole('treeitem', { name: /customer-order review/ }).click();
  await knowledge.getByRole('treeitem', { name: /order-4500131/ }).click();
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  const order = wv.locator('escurel-page-as-ui');
  await expect(order.getByText('Sales document (VBELN)')).toBeVisible();
  await expect(order.getByText('Customer PO (BSTNK)')).toBeVisible();
  await expect(order.locator('table thead th', { hasText: 'Material' })).toBeVisible();
  await expect(order.locator('table tbody tr')).toHaveCount(2);
  await stack.shot('06-order-page');
});

test('a failed run is listed under Dead letters, and Requeue is there but deactivated for a human', async ({
  stack,
}) => {
  const { page } = stack;
  await stack.call('capture_event', {
    label_skill: 'supplier-risk',
    instance_page_id: 'markdown/instances/customer-order__order-4500152.md',
    title: 'Provoked failure',
    body: 'a harness the runner does not allow',
    mime: 'text/plain',
    source: 'e2e',
    provenance: { manual: { mode: 'run', harness: 'no-such-harness' } },
  });
  const runner = pane(page, 'Runner');
  const dead = runner.getByRole('treeitem', { name: /order-4500152/ });
  await expect(dead).toBeVisible({ timeout: 60_000 });
  await dead.click({ button: 'right' });
  const requeue = page.getByRole('menuitem', { name: /Requeue/ });
  await expect(requeue).toBeVisible();
  await expect(requeue).toHaveAttribute('aria-disabled', 'true');
  await expect(page.getByRole('menuitem', { name: /Retry run/ })).toBeVisible();
  await stack.shot('07-dead-letter-menu');
  await page.keyboard.press('Escape');
});

test('a live run can be cancelled from its run detail', async ({ stack }) => {
  const { page } = stack;
  await stack.call('capture_event', {
    label_skill: 'supplier-risk',
    instance_page_id: 'markdown/instances/customer-order__order-4500140.md',
    title: 'Cancel me',
    body: 'a run that idles long enough to be cancelled',
    mime: 'text/plain',
    source: 'e2e',
    provenance: { manual: { mode: 'run' } },
  });
  const runner = pane(page, 'Runner');
  const live = runner.getByRole('treeitem', { name: /order-4500140/ });
  await expect(live).toBeVisible({ timeout: 60_000 });
  await live.click();
  const run = await webviewWith(page, 'escurel-run-detail');
  const cancel = run.getByRole('button', { name: 'Cancel run' });
  await expect(cancel).toBeVisible();
  await stack.shot('08-cancel-offered');
  await cancel.click();
  await expect(run.locator('.status-chip')).toContainText('cancelled', { timeout: 30_000 });
  await stack.shot('09-cancelled');
});

test('nothing in the extension threw while all of that happened', async ({ stack }) => {
  const ours = stack.errors.filter((e) => /escurel|pageerror/i.test(e));
  expect(ours).toEqual([]);
});
