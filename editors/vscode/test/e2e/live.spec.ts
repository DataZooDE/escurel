import type { Page } from '@playwright/test';
import { expect, test, webviewWith } from './fixtures';

// One window for the whole file, played in order: each scenario leaves the stack as the next one can
// use it. Every one asserts what a person would SEE and leaves a screenshot in artifacts/ for a human
// to look at, because "the assertion passed" says nothing about whether it looks right.
test.describe.configure({ mode: 'serial' });

const pane = (page: Page, title: string) =>
  page.locator('.pane', { has: page.locator('.pane-header', { hasText: title }) });

/**
 * The tree views only render the rows in view (the list is virtualised), and the Knowledge tree now
 * holds folders, so a row may not exist until it is scrolled to. Scroll from the top, a step at a time,
 * until the row is rendered; no test depends on how tall the window happens to be.
 */
async function knowledgeRow(page: Page, name: RegExp) {
  const k = pane(page, 'Knowledge');
  const row = k.getByRole('treeitem', { name });
  const list = k.locator('.monaco-list').first();
  await list.hover();
  await page.mouse.wheel(0, -10_000);
  for (let i = 0; i < 40 && (await row.count()) === 0; i += 1) {
    await page.mouse.wheel(0, 120);
    await page.waitForTimeout(120);
  }
  await expect(row.first()).toBeVisible();
  return row.first();
}

/** A skill row by what a screen reader hears: its role, then its id. */
const skillRow = (page: Page, id: string) => knowledgeRow(page, new RegExp(`skill ${id},`));

test('the story is on screen: knowledge, threads, awaiting, inbox and the runner', async ({
  stack,
}) => {
  const { page } = stack;
  // The Knowledge tree: skills sit in folders from their `folder:`, with a role in the name a screen
  // reader hears; the plumbing folder starts collapsed.
  await expect(await knowledgeRow(page, /^folder sales\/orders$/)).toBeVisible();
  await expect(await skillRow(page, 'customer-order')).toHaveAttribute(
    'aria-label',
    /^record skill customer-order/,
  );
  await expect(await skillRow(page, 'supplier-risk')).toHaveAttribute(
    'aria-label',
    /^process skill supplier-risk/,
  );
  await expect(await skillRow(page, 'supplier-risk-report')).toHaveAttribute(
    'aria-label',
    /^report skill supplier-risk-report/,
  );
  // Helpers are tucked away: the plumbing folder starts collapsed.
  await expect(await knowledgeRow(page, /^folder plumbing$/)).toHaveAttribute(
    'aria-expanded',
    'false',
  );
  await (await knowledgeRow(page, /^folder sales\/orders$/)).scrollIntoViewIfNeeded();
  await stack.shot('01b-knowledge-tree');
  await expect(pane(page, 'Awaiting You').getByRole('treeitem').first()).toBeVisible();
  await expect(pane(page, 'Inbox').getByRole('treeitem').first()).toBeVisible();
  await expect(pane(page, 'Runner').getByRole('treeitem', { name: /Health ok/ })).toBeVisible();
  // First: a live run's row names the harness too, so there can be two.
  await expect(
    pane(page, 'Runner')
      .getByRole('treeitem', { name: /harness: echo/ })
      .first(),
  ).toBeVisible();
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

test('work that waits on a person stands out: a Needs-you changeset with its drafts and buttons', async ({
  stack,
}) => {
  const { page } = stack;
  // The story's second thread: a supplier-risk run proposed a changeset that nobody has decided.
  await pane(page, 'Inbox')
    .getByRole('treeitem', { name: /order-4500131/ })
    .first()
    .click();
  const wv = await webviewWith(page, 'escurel-thread-canvas');
  const canvas = wv.locator('escurel-thread-canvas');
  // First view: the node that needs you is on screen when the thread opens, at 100%, without Fit.
  await expect(canvas.locator('.card.type-changeset.needs-you')).toBeInViewport({ ratio: 1 });
  await expect(canvas.locator('.zoom-level')).toHaveText('100%');
  await stack.shot('02a-first-view');
  await wv.getByRole('button', { name: 'Fit' }).click();
  const card = canvas.locator('.card.type-changeset.needs-you');
  await expect(card).toBeVisible();
  await expect(card.locator('.needs-badge')).toContainText('Needs you');
  await expect(card.locator('.changeset-author')).toContainText('agent:supplier-risk');
  // The pages it changes are listed on the card, each one openable; the order and the new analysis.
  await expect(card.locator('.draft-entry')).toHaveCount(2);
  await expect(card.getByRole('button', { name: /Promote all 2/ })).toBeVisible();
  await expect(card.getByRole('button', { name: 'Discard' })).toBeVisible();
  await expect(card.getByRole('button', { name: 'Review changes' })).toBeVisible();
  await stack.shot('02b-needs-you');
  // Review changes opens the same changeset review as Awaiting You: its picker of what to decide.
  await card.getByRole('button', { name: 'Review changes' }).click();
  const picker = page.locator('.quick-input-widget');
  await expect(picker).toBeVisible();
  await expect(picker).toContainText('Promote all');
  await stack.shot('02c-review-picker');
  await page.keyboard.press('Escape');
  await expect(picker).toBeHidden();
});

test('zoomed out, cards keep icon, accent and state but drop their words', async ({ stack }) => {
  const { page } = stack;
  const wv = await webviewWith(page, 'escurel-thread-canvas');
  const canvas = wv.locator('escurel-thread-canvas');
  for (let i = 0; i < 6; i += 1) await wv.getByRole('button', { name: 'Zoom out' }).click();
  await expect(canvas.locator('.canvas-area.low-zoom')).toBeVisible();
  await expect(canvas.locator('.zoom-hint')).toHaveText('overview');
  const card = canvas.locator('.card.type-changeset.needs-you');
  await expect(card.locator('.type-icon svg')).toBeVisible();
  await expect(card.locator('.needs-badge svg')).toBeVisible();
  await expect(card.locator('.card-title')).toBeHidden();
  // The accessible name still carries everything.
  await expect(card).toHaveAttribute('aria-label', /changeset.*needs you/i);
  await stack.shot('02d-overview');
  // Back to readable text for the scenarios that follow.
  for (let i = 0; i < 6; i += 1) await wv.getByRole('button', { name: 'Zoom in' }).click();
  await expect(canvas.locator('.canvas-area.low-zoom')).toHaveCount(0);
});

test('a sales order opens as a real order page: SAP fields and an items table', async ({
  stack,
}) => {
  const { page } = stack;
  await (await skillRow(page, 'customer-order')).click();
  await (await knowledgeRow(page, /order-4500131/)).click();
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
  await (await knowledgeRow(page, /order-4500131/)).click();
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
  // Earlier scenarios left customer-order expanded: fold it, then open the analysis skill and its page
  // (the analysis the first run wrote and the demo promoted together with its change to the order; its
  // id is the supplier and the day: meier-guss-YYYY-MM-DD).
  const orders = await skillRow(page, 'customer-order');
  if ((await orders.getAttribute('aria-expanded')) === 'true') await orders.click();
  await (await skillRow(page, 'supplier-risk-analysis')).click();
  await (await knowledgeRow(page, /^meier-guss-\d{4}-\d{2}-\d{2}/)).click();
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

test('a SQL-view page previews the rows the source holds, read-only, under its form', async ({
  stack,
}) => {
  const { page } = stack;
  // The helper folders start collapsed: open plumbing > sap > order-lines > all.
  await (await knowledgeRow(page, /^folder plumbing$/)).click();
  await (await knowledgeRow(page, /^folder plumbing\/sap$/)).click();
  await expect(await skillRow(page, 'order-lines')).toHaveAttribute(
    'aria-label',
    /^helper skill order-lines/,
  );
  await (await skillRow(page, 'order-lines')).click();
  await (await knowledgeRow(page, /^all/)).click();
  const wv = await webviewWith(page, 'escurel-source-preview');
  const preview = wv.locator('escurel-page-as-ui escurel-source-preview');
  await expect(preview.locator('.badge')).toContainText('read-only (source)');
  // The rows come from the real gateway's `expand` (the sql_view over the JSON extract).
  await expect(preview.locator('thead th')).toContainText(['order_id', 'item', 'customer']);
  await expect(preview.locator('tbody tr')).toHaveCount(6);
  await expect(preview.locator('tbody')).toContainText('GH-4711');
  await stack.shot('06d-sql-view-preview');
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
