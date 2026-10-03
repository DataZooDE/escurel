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
  // event -> run -> changeset -> its two instances (the order and the run's analysis) -> ONE follow-on
  // event -> its run. The analysis is part of the run's output, not a cascade hop of its own.
  await expect(canvas.getByRole('treeitem')).toHaveCount(7);
  // Finished nodes are small; every card says what it is with an icon and a word; the open top-left
  // starts under the headers instead of floating in the middle.
  await expect(canvas.locator('.card.compact').first()).toBeVisible();
  await expect(canvas.locator('.card.type-event .type-label').first()).toHaveText('event');
  await expect(canvas.locator('.card.type-run .type-label').first()).toHaveText('run');
  await expect(canvas.locator('.card.type-changeset .type-label').first()).toHaveText('changeset');
  await expect(canvas.locator('.card .type-icon svg').first()).toBeVisible();
  await stack.shot('02-thread');

  // The instance a draft proposes a change to: select it, and the inspector offers its skill's actions.
  await canvas.getByRole('treeitem', { name: /order-4500123/ }).click();
  const start = wv.getByRole('group', { name: 'Skills' }).locator('.primary').first();
  await expect(start).toBeVisible();
  await stack.shot('03-thread-inspector-instance');

  // Click it. A start event appears in the Inbox, and the runner takes it.
  const inbox = pane(page, 'Inbox').getByRole('treeitem', {
    name: /^(?=.*order-4500123)(?=.*supplier-risk)/,
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
  // The run card, by its type: a finished card is small and no longer shows its meta lines.
  await wv.locator('escurel-thread-canvas').locator('.card.type-run').first().dblclick();
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

test('a wikilink in the order opens the page it names', async ({ stack }) => {
  const { page } = stack;
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  await wv.getByRole('button', { name: 'Meier-Guss GmbH' }).click();
  // The supplier opens as its own page, in a tab of its own that becomes the active one.
  await expect(page.getByRole('tab', { name: /supplier__meier-guss/, selected: true })).toBeVisible(
    { timeout: 20_000 },
  );
  const supplier = await webviewWith(page, 'escurel-page-as-ui');
  // The first h1 is the page's title (a body can carry its own h1 further down).
  await expect(supplier.getByRole('heading', { level: 1 }).first()).toContainText('Meier-Guss');
  await stack.shot('06b-wikilink-opened');
  // Back to the order, as the following scenarios expect.
  await pane(page, 'Knowledge')
    .getByRole('treeitem', { name: /order-4500131/ })
    .click();
});

test('the Skill menu works from the keyboard alone, and Escape gives the focus back', async ({
  stack,
}) => {
  const { page } = stack;
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  const chevron = wv.getByRole('button', { name: /More actions for / }).first();
  await chevron.focus();
  await page.keyboard.press('ArrowDown');
  const menu = wv.getByRole('menu').first();
  await expect(menu).toBeVisible();
  // The four ways to start, in the order the spec fixes, with the first one focused.
  await expect(menu.getByRole('menuitem')).toHaveText([
    'Start in background',
    'First make a plan',
    'Start in terminal',
    'View skill',
  ]);
  await expect(menu.getByRole('menuitem').first()).toBeFocused();
  // Every item can be reached on screen, even though this page is scrolled to its end.
  await expect(menu.getByRole('menuitem').last()).toBeInViewport({ ratio: 1 });
  await stack.shot('07-skill-menu-keyboard');
  await page.keyboard.press('ArrowDown');
  await expect(menu.getByRole('menuitem').nth(1)).toBeFocused();
  await page.keyboard.press('Escape');
  await expect(menu).toHaveCount(0);
  await expect(chevron).toBeFocused();
});

test('"First make a plan" ends in a plan the person is asked to approve', async ({ stack }) => {
  const { page } = stack;
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  const chevron = wv.getByRole('button', { name: /More actions for / }).first();
  await chevron.focus();
  await page.keyboard.press('ArrowDown');
  await wv.getByRole('menuitem', { name: 'First make a plan' }).click();
  // The runner drafts a plan and stops; VS Code asks, in a notification, whether to carry it out.
  const toast = page.locator('.notification-toast', { hasText: /Plan ready for/ });
  await expect(toast).toBeVisible({ timeout: 60_000 });
  await expect(toast.getByRole('button', { name: 'Approve plan' })).toBeVisible();
  await stack.shot('08-plan-ready');
  // Not now: nothing is run behind the person's back.
  await page.keyboard.press('Escape');
});

test('a supplier-risk run leaves an analysis: fields, a text alternative for its chart, and follow-ups', async ({
  stack,
}) => {
  const { page } = stack;
  const knowledge = pane(page, 'Knowledge');
  // Earlier scenarios left customer-order expanded; the tree only renders the rows in view, so fold it.
  const orders = knowledge.getByRole('treeitem', { name: /^customer-order/ }).first();
  if ((await orders.getAttribute('aria-expanded')) === 'true') await orders.click();
  await knowledge.getByRole('treeitem', { name: /^supplier-risk-analysis/ }).click();
  // The analysis the first run wrote and the demo promoted together with its change to the order
  // (its id is the supplier and the day: meier-guss-YYYY-MM-DD).
  await knowledge.getByRole('treeitem', { name: /^meier-guss-\d{4}-\d{2}-\d{2}/ }).click();
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  const analysis = wv.locator('escurel-page-as-ui');
  await expect(analysis.locator('.field[data-name="risk_level"]')).toContainText('high');
  await expect(analysis.locator('.field[data-name="net_value_at_risk"]')).toContainText(
    '128,600.00',
  );
  // No raw event id in the form; the page says in words which signal it answers.
  await expect(analysis.locator('.field[data-name="source_event"]')).toHaveCount(0);
  await expect(analysis.getByText(/Triggered by/)).toBeVisible();
  // The chart's text alternative: a plain sentence with the takeaway, and the table behind the chart.
  await expect(analysis.getByText(/2 orders are affected and carry 128,600\.00 EUR/)).toBeVisible();
  await expect(analysis.locator('table thead th', { hasText: 'Net value (EUR)' })).toBeVisible();
  await expect(analysis.locator('table tbody tr')).toHaveCount(2);
  // Follow-ups are labelled by what the skill declares; the prompt-kind action is not offered here.
  await expect(
    analysis.locator('.skill-button .primary', { hasText: /Notify the affected customers/ }),
  ).toBeVisible();
  await expect(
    analysis.locator('.skill-button .primary', {
      hasText: /Ask the supplier for a new confirmation/,
    }),
  ).toBeVisible();
  await expect(
    analysis.locator('.skill-button .primary', { hasText: /Why is this risky/ }),
  ).toHaveCount(0);
  await stack.shot('06c-supplier-risk-analysis');
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

test('a failed run can be retried from the Runner view, and the person is told what happened', async ({
  stack,
}) => {
  const { page } = stack;
  const dead = pane(page, 'Runner').getByRole('treeitem', { name: /order-4500152/ });
  await dead.click({ button: 'right' });
  // VS Code's own context menu acts on Enter; a synthetic click on its item is not reliable.
  await page.getByRole('menuitem', { name: /Retry run/ }).hover();
  await page.keyboard.press('Enter');
  // The request goes out as an event; the runner answers it; the person is told the outcome in words.
  // First the person is told the request is out, then what the runner answered, in words and with no id.
  await expect(
    page.locator('.notification-toast', { hasText: /Waiting for runner to retry/ }),
  ).toBeVisible();
  const answer = page.locator('.notification-toast', { hasText: /Retried; a new run has started/ });
  await expect(answer).toBeVisible({ timeout: 40_000 });
  expect(await answer.innerText()).not.toMatch(/[0-9A-Z]{20,}/);
  await stack.shot('07b-retry-answer');
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
