import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';
import { join } from 'node:path';
import type { FrameLocator, Page } from '@playwright/test';
import { expect, test, webviewWith, type Stack } from './fixtures';
import { chooseMenuItem, knowledgeRow, openRow, pane, skillRow } from './helpers';

// One window for the whole file, played in order: each scenario leaves the stack as the next one can
// use it. Every one asserts what a person would SEE and leaves a screenshot in artifacts/ for a human
// to look at, because "the assertion passed" says nothing about whether it looks right.
test.describe.configure({ mode: 'serial' });

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
  // The runner is described in plain words, and dispatch is a row of its own.
  await expect(pane(page, 'Runner').getByText(/Runner ok · echo harness/)).toBeVisible();
  await expect(
    pane(page, 'Runner').getByRole('treeitem', { name: /Dispatch is on/ }),
  ).toBeVisible();
  await stack.shot('01-overview');
});

test('the Knowledge tree can be narrowed by a tag and cleared again', async ({ stack }) => {
  const { page } = stack;
  const knowledge = pane(page, 'Knowledge');
  // View title actions show while the pointer is over the view's header.
  await knowledge.locator('.pane-header').hover();
  await knowledge.getByRole('button', { name: /Filter knowledge by tag/ }).click();
  const picker = page.locator('.quick-input-widget');
  await expect(picker).toBeVisible();
  await page.keyboard.type('sap');
  await expect(picker.getByRole('option', { name: /sap/ }).first()).toBeVisible();
  await page.keyboard.press('Enter');
  // The view says what narrows it and how much is left, in words.
  await expect(knowledge.getByText(/Filtered by tag: sap — \d+ skills?/)).toBeVisible();
  await expect(await skillRow(page, 'customer-order')).toBeVisible();
  await expect(knowledge.getByRole('treeitem', { name: /skill supplier-risk,/ })).toHaveCount(0);
  await stack.shot('01c-knowledge-filtered');
  await knowledge.locator('.pane-header').hover();
  await knowledge.getByRole('button', { name: /Clear knowledge filter/ }).click();
  await expect(knowledge.getByText(/Filtered by/)).toHaveCount(0);
  await expect(await skillRow(page, 'supplier-risk')).toBeVisible();
});

test('the thread shows the cascade, and an instance offers a skill to start', async ({ stack }) => {
  const { page } = stack;
  // The previous scenario left the pointer over a Knowledge row; its tooltip would cover the canvas.
  await page.mouse.move(1000, 700);
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

  // The instance a draft proposes a change to: select it, and the DETAILS view, a view of its own in
  // the bottom panel (VS Code lays it out), shows the node and offers its skill's actions.
  await canvas.getByRole('treeitem', { name: /^page: order-4500123/ }).click();
  const details = await webviewWith(page, 'escurel-details');
  await expect(details.locator('escurel-details')).toContainText('order-4500123');
  const start = details.getByRole('group', { name: 'Skills' }).locator('.primary').first();
  await expect(start).toBeVisible();
  // The canvas keeps its full width: the inspector is no longer a column inside it.
  await expect(canvas.locator('escurel-thread-inspector')).toHaveCount(0);
  await expect(page.getByRole('tab', { name: /Escurel Details/ })).toBeVisible();
  // The panel opens with a SENTENCE, not a key/value dump, and the engineer fields are collapsed.
  await expect(details.locator('escurel-thread-inspector .summary')).toBeVisible();
  await expect(details.locator('escurel-thread-inspector .kind')).toBeVisible();
  await stack.shot('03-thread-inspector-instance');

  // Click it. A start event appears in the Inbox, and the runner takes it.
  const inbox = pane(page, 'Inbox').getByRole('treeitem', {
    name: /^(?=.*order-4500123)(?=.*supplier-risk)/,
  });
  const before = await inbox.count();
  await start.click();
  await expect.poll(() => inbox.count()).toBeGreaterThan(before);
  // Routine feedback is a fading status-bar message, not a toast that covers the panel.
  await expect(page.locator('.statusbar')).toContainText('Started supplier-risk');
  await expect(page.locator('.notification-toast', { hasText: /^Started / })).toHaveCount(0);
  await stack.shot('04-after-start');
});

test('run detail opens from the canvas with its plan and tool calls', async ({ stack }) => {
  const { page } = stack;
  // Scenario 2 started a skill, which opened that event's thread in a NEW tab. Go back to the story's
  // thread, the one with a finished run, as a person would.
  await page.locator('.tab', { hasText: 'PO 4500087412 confirmation' }).first().click();
  const wv = await webviewWith(page, 'escurel-thread-canvas');
  // The canvas is panned to whatever was selected last; Fit brings every card back into view.
  await wv.getByRole('button', { name: 'Fit' }).click();
  // The run card, by its type: a finished card is small and no longer shows its meta lines.
  await wv.locator('escurel-thread-canvas').locator('.card.type-run').first().dblclick();
  await expect(page.locator('.tab .label-name', { hasText: /^Run / })).toBeVisible();
  const run = await webviewWith(page, 'escurel-run-detail');
  await expect(run.locator('escurel-run-detail h1')).toContainText('supplier-risk on ');
  await expect(run.getByText('Tool calls')).toBeVisible();
  // Plain words, and the 26-character id stays behind Copy run id.
  await expect(run.locator('escurel-run-detail .meta')).toContainText('Run by the');
  await expect(run.getByRole('button', { name: 'Copy run id' })).toBeVisible();
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
  // Titled by the decision and who proposed it (no ULID), with rows that name the pages.
  await expect(picker.locator('input')).toHaveAttribute(
    'placeholder',
    'Review 2 changes from agent:supplier-risk',
  );
  await expect(picker).toContainText('Apply all changes');
  await expect(picker).not.toContainText(/[0-9A-Z]{26}/);
  await stack.shot('02c-review-picker');
  await page.keyboard.press('Escape');
  await expect(picker).toBeHidden();
});

test('zoomed out, cards keep icon, type, title and state but drop their body', async ({
  stack,
}) => {
  const { page } = stack;
  const wv = await webviewWith(page, 'escurel-thread-canvas');
  const canvas = wv.locator('escurel-thread-canvas');
  for (let i = 0; i < 6; i += 1) await wv.getByRole('button', { name: 'Zoom out' }).click();
  await expect(canvas.locator('.canvas-area.low-zoom')).toBeVisible();
  await expect(canvas.locator('.zoom-hint')).toHaveText('overview');
  const card = canvas.locator('.card.type-changeset.needs-you');
  await expect(card.locator('.type-icon svg')).toBeVisible();
  await expect(card.locator('.needs-badge svg')).toBeVisible();
  // The words stay, at a readable size: type word and title are visible, the body is not.
  await expect(card.locator('.card-title')).toBeVisible();
  await expect(card.locator('.type-label')).toBeVisible();
  await expect(card.locator('.draft-list')).toBeHidden();
  const rendered = await card
    .locator('.card-title')
    .evaluate(
      (el, z) => parseFloat(getComputedStyle(el).fontSize) * z,
      await canvas.evaluate((c) => (c as unknown as { viewport: { zoom: number } }).viewport.zoom),
    );
  expect(rendered).toBeGreaterThanOrEqual(9.5);
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
  await expect(order.locator('.body table thead th', { hasText: 'Material' })).toBeVisible();
  await expect(order.locator('.body table tbody tr')).toHaveCount(2);
  // The order is ONE ROW of the SAP extract (read-only) plus the person's own notes, and says so.
  const strip = order.locator('.source-strip');
  await expect(strip).toContainText('Read-only copy from');
  await expect(strip).toContainText(/read-only/i);
  await expect(order.locator('.field[data-source="true"]')).not.toHaveCount(0);
  // The delivery risk is the notes' own field, not a source column.
  await expect(order.locator('.field[data-name="delivery_risk"]')).not.toHaveAttribute(
    'data-source',
    'true',
  );
  // One clear action for the person's own notes, and the page details are folded away.
  await expect(strip.getByRole('button', { name: /^(Add|Edit) notes?$/ })).toBeVisible();
  await expect(order.locator('details.page-meta')).not.toHaveAttribute('open', '');
  await expect(order.locator('.readonly-note')).toHaveCount(0);
  // The Details panel follows the editor in front: beside an order page it is empty, not stale.
  const details = await webviewWith(page, 'escurel-details');
  await expect(details.locator('escurel-details .empty')).toBeVisible();
  await stack.shot('06-order-page');
});

test('the Markdown view of an order is its notes only, never the SAP columns', async ({
  stack,
}) => {
  const { page } = stack;
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  await wv.getByRole('button', { name: 'Markdown' }).click();
  // The raw editor opens on the notes: the companion's frontmatter and body, with no row column in it.
  const editor = page.locator('.monaco-editor').first();
  await expect(editor).toBeVisible({ timeout: 20_000 });
  await expect(editor).toContainText('delivery_risk');
  await expect(editor).not.toContainText('sold_to_name');
  await stack.shot('06a-order-notes-markdown');
  // Back to the page view for the scenarios that follow.
  await page.keyboard.press('Control+w');
});

test('a wikilink in the order opens the page it names', async ({ stack }) => {
  const { page } = stack;
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  await wv.getByRole('button', { name: 'Meier-Guss GmbH' }).click();
  // The supplier opens as its own page, in a tab of its own that becomes the active one.
  await expect(page.getByRole('tab', { name: /meier-guss/, selected: true })).toBeVisible({
    timeout: 20_000,
  });
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
  // A dismissed toast must not lose the plan: the planned run's card in the thread has the button too.
  await page.keyboard.press('Escape');
  const canvas = (await webviewWith(page, 'escurel-thread-canvas')).locator(
    'escurel-thread-canvas',
  );
  await expect(canvas.locator('.card.needs-you button.approve-btn')).toBeVisible({
    timeout: 30_000,
  });
  await stack.shot('08b-approve-on-card');
  // The wait is also a row in Awaiting You, so it survives a dismissed toast and a closed thread.
  const planRow = pane(page, 'Awaiting You').getByRole('treeitem', { name: /Plan ready/ });
  await expect(planRow.first()).toBeVisible({ timeout: 30_000 });
  await stack.shot('08c-plan-row-in-awaiting');
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
  await openRow(page, 'supplier-risk-analysis', /^meier-guss-\d{4}-\d{2}-\d{2}/);
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

test('a failed run is listed under Needs attention, and Requeue is there but deactivated for a human', async ({
  stack,
}) => {
  const { page } = stack;
  await stack.call('capture_event', {
    label_skill: 'supplier-risk',
    instance_page_id: 'markdown/instances/customer-order/order-4500152.md',
    title: 'Provoked failure',
    body: 'a harness the runner does not allow',
    mime: 'text/plain',
    source: 'e2e',
    provenance: { manual: { mode: 'run', harness: 'no-such-harness' } },
  });
  const runner = pane(page, 'Runner');
  // Needs attention comes before History in the tree, so the first match is the one that can be acted on.
  const dead = runner.getByRole('treeitem', { name: /order-4500152/ }).first();
  await expect(dead).toBeVisible({ timeout: 60_000 });
  // A short word on screen, the whole one for a screen reader.
  await expect(dead).toContainText(/gave up/);
  await expect(dead).toHaveAttribute(
    'aria-label',
    /^failed for good: supplier-risk · order-4500152/,
  );
  // And the reason is its own line under it, in words.
  await expect(runner.getByRole('treeitem', { name: /Reason: permanent — harness/ })).toBeVisible();
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
  const dead = pane(page, 'Runner')
    .getByRole('treeitem', { name: /order-4500152/ })
    .first();
  await dead.click({ button: 'right' });
  // VS Code's own context menu acts on Enter; a synthetic click on its item is not reliable.
  await page.getByRole('menuitem', { name: /Retry run/ }).hover();
  await page.keyboard.press('Enter');
  // The request goes out as an event; the runner answers it; the person is told the outcome in words.
  // The person is told the request is out (a progress toast) and then what the runner answered, in words
  // and with no id. The progress toast lives only as long as the runner takes to answer, which can be
  // shorter than a poll: it is accepted when seen, but only the answer is required.
  const progress = page.locator('.notification-toast', { hasText: /Waiting for runner to retry/ });
  const answer = page.locator('.notification-toast', { hasText: /Retried; a new run has started/ });
  await expect(progress.or(answer).first()).toBeVisible({ timeout: 40_000 });
  await expect(answer).toBeVisible({ timeout: 40_000 });
  expect(await answer.innerText()).not.toMatch(/[0-9A-Z]{20,}/);
  // A notice that names a run offers to open it.
  await expect(answer.getByRole('button', { name: 'Open run' })).toBeVisible();
  await stack.shot('07b-retry-answer');
});

test('a live run can be cancelled from its run detail', async ({ stack }) => {
  const { page } = stack;
  await stack.call('capture_event', {
    label_skill: 'supplier-risk',
    instance_page_id: 'markdown/instances/customer-order/order-4500140.md',
    title: 'Cancel me',
    body: 'a run that idles long enough to be cancelled',
    mime: 'text/plain',
    source: 'e2e',
    provenance: { manual: { mode: 'run' } },
  });
  const runner = pane(page, 'Runner');
  const live = runner.getByRole('treeitem', { name: /order-4500140/ }).first();
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

test('the Runner panel is a control center: sections with counts, an insight line, a filter, and no ids', async ({
  stack,
}) => {
  const { page } = stack;
  const runner = pane(page, 'Runner');
  // Sections, each with its count, in words.
  await expect(runner.getByRole('treeitem', { name: /^Running now/ })).toBeVisible();
  await expect(runner.getByRole('treeitem', { name: /^Needs attention/ })).toBeVisible();
  const history = runner.getByRole('treeitem', { name: /^History/ });
  await expect(history).toBeVisible();
  // The insight line sums the day up.
  await expect(runner.getByText(/Last 24 h: \d+ runs? · \d+ ok/)).toBeVisible();
  // A run says what it did, to what, in words: status as a word, never a bare id.
  const cancelled = runner.getByRole('treeitem', { name: /cancelled: .*order-4500140/ }).first();
  await expect(cancelled).toBeVisible();
  for (const row of await runner.getByRole('treeitem').all()) {
    expect(await row.innerText()).not.toMatch(/[0-9A-Z]{20,}/);
  }
  await stack.shot('10-runs-panel');

  // Filter History to what failed: a title action opens a pick list; nothing else has to change.
  // The Runner is alone in its container, so its title actions sit in the container's title bar.
  await page.getByRole('button', { name: /Filter runs/ }).click();
  const picker = page.locator('.quick-input-widget');
  await expect(picker).toBeVisible();
  await picker.getByRole('checkbox', { name: /^Failed/ }).first().click();
  await page.keyboard.press('Enter');
  await expect(runner.getByText(/Filtered: failed/)).toBeVisible();
  await expect(runner.getByRole('treeitem', { name: /cancelled: / })).toHaveCount(0);
  await stack.shot('10b-runs-filtered');
  await page.getByRole('button', { name: /Clear run filter/ }).click();
  await expect(runner.getByText(/Filtered:/)).toHaveCount(0);
});

// --- outside systems: a REST portal and an MCP server, real processes the demo started -------------

/** The base URL of one of the demo's outside systems (the launcher wrote its port next to its pid). */
const outside = (home: string, name: 'ratings' | 'confirmations'): string => {
  const port = JSON.parse(readFileSync(join(home, `${name}.json`), 'utf8').split('\n')[0]!).port;
  return `http://127.0.0.1:${port}`;
};

type DraftRef = { draft_id: string; target_page_id: string };

/** The proposal for a page, once it exists: a toast from an earlier step may still be on screen. */
async function waitForDraft(stack: Stack, targetSuffix: string): Promise<DraftRef> {
  let found: DraftRef | undefined;
  await expect
    .poll(
      async () => {
        const out = (await stack.call('list_drafts', {})) as { drafts: DraftRef[] };
        found = out.drafts.find((d) => d.target_page_id.includes(targetSuffix));
        return found?.draft_id;
      },
      { message: `a proposal for ${targetSuffix} is waiting for a reviewer`, timeout: 20_000 },
    )
    .toBeTruthy();
  return found!;
}

async function answerQuickInput(page: Page, title: RegExp, text: string) {
  // Wait for THIS prompt: the next one opens as the previous closes, and typing too early answers the
  // wrong one.
  await expect(page.locator('.quick-input-title')).toHaveText(title);
  const input = page.locator('.quick-input-widget input.input');
  await input.fill(text);
  await page.keyboard.press('Enter');
}

test('a document is chunked and previewed, and its original is saved as plain text, never run', async ({
  stack,
}) => {
  const { page } = stack;
  await expect(await skillRow(page, 'supplier-document')).toBeVisible();
  await openRow(page, 'supplier-document', /frame agreement/i);
  const wv = await webviewWith(page, 'escurel-page-as-ui', 'Frame agreement');
  const preview = wv.locator('escurel-source-preview');
  await expect(preview).toContainText('read-only (source)');
  // The extracted text, chunk by chunk, as the source holds it.
  await expect(preview.locator('.chunk').first()).toBeVisible();
  await expect(preview).toContainText(/delivery terms|7 days/i);
  await expect(preview).toContainText(/Showing \d+ of \d+ chunks/);
  await stack.shot('06e-document-preview');

  // The uploaded file is untrusted: markdown is never handed to an application. The person is told
  // so, the file lands in the extension's storage as `.txt`, and nothing else is opened.
  await preview.getByRole('button', { name: 'Open original' }).click();
  await expect(
    page.locator('.notification-toast', { hasText: /not opened from here/ }),
  ).toBeVisible({ timeout: 20_000 });
  const originals = join(
    stack.home,
    'profile',
    'User',
    'globalStorage',
    'datazoo.escurel',
    'originals',
  );
  await expect
    .poll(() => (existsSync(originals) ? readdirSync(originals) : []), {
      message: `the original was saved under ${originals}`,
    })
    .toEqual([expect.stringMatching(/^[a-z0-9._-]+-[0-9a-f]{12}\.txt$/)]);
  const saved = readFileSync(join(originals, readdirSync(originals)[0]!), 'utf8');
  expect(saved).toContain('Frame agreement Meier-Guss GmbH');
  await stack.shot('06f-document-original-saved');
});

test('the two outside systems are in the tree, and a REST row says it is external data', async ({
  stack,
}) => {
  const { page } = stack;
  await expect(await skillRow(page, 'supplier-rating')).toBeVisible();
  await expect(await skillRow(page, 'delivery-confirmation')).toBeVisible();

  await openRow(page, 'supplier-rating', /iberica-forja/);
  const wv = await webviewWith(page, 'escurel-page-as-ui', 'iberica-forja');
  const strip = wv.locator('.source-strip');
  await expect(strip).toContainText('External data (REST)');
  await expect(strip).toContainText(/read-only/i);
  // The portal's own columns, live from the real service.
  await expect(wv.locator('.field[data-name="display_name"]')).toContainText('Ibérica Forja S.L.');
  await expect(wv.locator('.field[data-name="rating"]')).toContainText('A');
  await expect(strip.getByRole('button', { name: 'Change rating…' })).toBeVisible();
  await stack.shot('10-rest-row');
});

test('a rating change is proposed from the page, approved by a reviewer, and then the portal changes', async ({
  stack,
}) => {
  const { page } = stack;
  const wv = await webviewWith(page, 'escurel-page-as-ui', 'iberica-forja');
  await wv.getByRole('button', { name: 'Change rating…' }).click();
  await answerQuickInput(page, /^Change \w+ in the source$/, 'B');
  await answerQuickInput(page, /^Note for the reviewer/, 'Three late deliveries in Q3.');
  await expect(
    page.locator('.notification-toast', { hasText: /Proposed: rating to B/ }),
  ).toBeVisible({ timeout: 20_000 });

  // Proposing touched nothing: the portal still says A.
  const base = outside(stack.home, 'ratings');
  expect((await (await fetch(`${base}/ratings/iberica-forja`)).json()).rating).toBe('A');

  // The reviewer promotes it (the same call the review UI makes).
  const mine = await waitForDraft(stack, 'iberica-forja.md');
  const done = await stack.call('promote_draft', { draft_id: mine.draft_id });
  expect(done.ok, JSON.stringify(done)).toBe(true);

  // NOW the real service has the change, and the page reads it back and says what happened.
  expect((await (await fetch(`${base}/ratings/iberica-forja`)).json()).rating).toBe('B');
  await openRow(page, 'supplier-rating', /nordform/);
  await openRow(page, 'supplier-rating', /iberica-forja/);
  const again = await webviewWith(page, 'escurel-page-as-ui', 'iberica-forja');
  await expect(again.locator('.field[data-name="rating"]')).toContainText('B', { timeout: 20_000 });
  await expect(again.locator('.write-back')).toContainText('applied');
  await stack.shot('11-write-back-applied');
});

test('an MCP row works the same way: external data, a proposed change, applied only after approval', async ({
  stack,
}) => {
  const { page } = stack;
  await openRow(page, 'delivery-confirmation', /PO-4500087433-10/);
  const wv = await webviewWith(page, 'escurel-page-as-ui', 'PO-4500087433-10');
  await expect(wv.locator('.source-strip')).toContainText('External data (MCP)');
  await expect(wv.locator('.field[data-name="status"]')).toContainText('open');
  await wv.getByRole('button', { name: 'Change status…' }).click();
  await answerQuickInput(page, /^Change \w+ in the source$/, 'confirmed');
  await answerQuickInput(page, /^Note for the reviewer/, 'Supplier confirmed by phone.');
  await expect(
    page.locator('.notification-toast', { hasText: /Proposed: status to confirmed/ }),
  ).toBeVisible({ timeout: 20_000 });
  const mine = await waitForDraft(stack, 'delivery-confirmation');
  const done = await stack.call('promote_draft', { draft_id: mine.draft_id });
  expect(done.ok, JSON.stringify(done)).toBe(true);
  // Read back through the MCP server: the status is what the reviewer approved.
  const page2 = (await stack.call('expand', {
    page_id: 'markdown/instances/delivery-confirmation/PO-4500087433-10.md',
  })) as { frontmatter: { status: string } };
  expect(page2.frontmatter.status).toBe('confirmed');
  await stack.shot('12-mcp-write-back');
});

test('a change based on a row that has moved is refused, and the portal is not touched', async ({
  stack,
}) => {
  const { page } = stack;
  await openRow(page, 'supplier-rating', /stahl-ag/);
  const wv = await webviewWith(page, 'escurel-page-as-ui', 'stahl-ag');
  await wv.getByRole('button', { name: 'Change rating…' }).click();
  await answerQuickInput(page, /^Change \w+ in the source$/, 'A');
  await answerQuickInput(page, /^Note for the reviewer/, 'Upgrade after the audit.');
  await expect(
    page.locator('.notification-toast', { hasText: /Proposed: rating to A/ }),
  ).toBeVisible({ timeout: 20_000 });

  // Someone else changes the supplier at the portal while the proposal waits for a reviewer.
  const base = outside(stack.home, 'ratings');
  const head = await fetch(`${base}/ratings/stahl-ag`);
  const patched = await fetch(`${base}/ratings/stahl-ag`, {
    method: 'PATCH',
    headers: { 'if-match': head.headers.get('etag')!, 'idempotency-key': 'someone-else' },
    body: JSON.stringify({ rating: 'C' }),
  });
  expect(patched.status).toBe(200);

  const mine = await waitForDraft(stack, 'stahl-ag.md');
  const refused = (await stack.call('promote_draft', { draft_id: mine.draft_id })) as {
    ok: boolean;
    issues: { code: string }[];
  };
  expect(refused.ok).toBe(false);
  expect(refused.issues.map((i) => i.code)).toContain('write_back_conflict');
  expect((await (await fetch(`${base}/ratings/stahl-ag`)).json()).rating).toBe('C'); // theirs, not ours
});

test('when the portal is down a promoted change is refused, recorded as failed, and the page says so', async ({
  stack,
}) => {
  const { page } = stack;
  await openRow(page, 'supplier-rating', /nordform/);
  const wv = await webviewWith(page, 'escurel-page-as-ui', 'nordform');
  await wv.getByRole('button', { name: 'Change rating…' }).click();
  await answerQuickInput(page, /^Change \w+ in the source$/, 'A');
  await answerQuickInput(page, /^Note for the reviewer/, 'Strong quarter.');
  await expect(
    page.locator('.notification-toast', { hasText: /Proposed: rating to A/ }),
  ).toBeVisible({ timeout: 20_000 });

  // The portal goes away (a real process, killed): nothing can be sent.
  const mine = await waitForDraft(stack, 'nordform.md');
  process.kill(Number(readFileSync(join(stack.home, 'ratings.pid'), 'utf8').trim()), 'SIGKILL');
  const failed = (await stack.call('promote_draft', { draft_id: mine.draft_id })) as {
    ok: boolean;
    issues: { code: string }[];
  };
  expect(failed.ok).toBe(false);
  expect(failed.issues.map((i) => i.code)).toContain('write_back_failed');
  // The draft is still waiting, so promoting again is the retry once the portal is back.
  const still = (await stack.call('list_drafts', {})) as { drafts: { draft_id: string }[] };
  expect(still.drafts.some((d) => d.draft_id === mine.draft_id)).toBe(true);
  // The page tells the person, in words: the source is unreachable (the portal is down), and the last
  // change did not go through. It keeps the page; it does not go blank or show a stack trace.
  // Its tab is still open; the tree cannot list a dead source's rows, so the person goes back to it.
  // (Switching away and back is what reloads a page; it is already the active tab.)
  await page.getByRole('tab', { name: /^stahl-ag\.md/ }).click();
  await page.getByRole('tab', { name: /^nordform\.md/ }).click();
  const down = await webviewWith(page, 'escurel-page-as-ui', 'nordform');
  await expect(down.locator('.source-strip')).toBeVisible({ timeout: 20_000 });
  await expect(down.locator('.source-strip.problem')).toBeVisible();
  await expect(down.locator('.write-back.problem')).toContainText('did not go through');
  // The banner promises "its values show as —": every source field shows the dash, none is blank, and
  // there is no empty pill where the rating badge was.
  for (const name of ['display_name', 'rating', 'on_time_pct', 'region']) {
    const cell = down.locator(`.field[data-name="${name}"]`);
    await expect(cell, name).toContainText('—');
    await expect(cell.locator('.badge'), `${name}: no empty pill`).toHaveCount(0);
  }
  await stack.shot('13-write-back-failed');
});

/** One row of the demo's real SQLite database, read straight from the file (not through escurel). */
const orderInDb = (home: string, orderNo: string): { status: string; qty: number } => {
  const db = new DatabaseSync(join(home, 'sqlite', 'orders.db'), { readOnly: true });
  try {
    return db.prepare('SELECT status, qty FROM orders WHERE order_no = ?').get(orderNo) as never;
  } finally {
    db.close();
  }
};

test('a row of a SQL database is changed through a proposal, only after a reviewer approves it', async ({
  stack,
}) => {
  const { page } = stack;
  await expect(await skillRow(page, 'orders-db')).toBeVisible();
  await openRow(page, 'orders-db', /SO-100231/);
  const wv = await webviewWith(page, 'escurel-page-as-ui', 'SO-100231');
  const strip = wv.locator('.source-strip');
  await expect(strip).toContainText('a SQL source');
  await expect(strip).not.toContainText('External data');
  await expect(wv.locator('.field[data-name="customer"]')).toContainText('Meier Gussteile');
  await expect(wv.locator('.field[data-name="status"]')).toContainText('open');
  await expect(strip.getByRole('button', { name: 'Change status…' })).toBeVisible();
  await expect(strip.getByRole('button', { name: 'Change quantity…' })).toBeVisible();
  // A column the skill does not list is not offered.
  await expect(strip.getByRole('button', { name: 'Change customer…' })).toHaveCount(0);
  await stack.shot('14-sql-row');

  await wv.getByRole('button', { name: 'Change status…' }).click();
  await answerQuickInput(page, /^Change \w+ in the source$/, 'shipped');
  await answerQuickInput(page, /^Note for the reviewer/, 'Left the warehouse today.');
  await expect(
    page.locator('.notification-toast', { hasText: /Proposed: status to shipped/ }),
  ).toBeVisible({ timeout: 20_000 });

  // Proposing touched nothing: the database file still says open.
  expect(orderInDb(stack.home, 'SO-100231')).toEqual({ status: 'open', qty: 40 });

  const mine = await waitForDraft(stack, 'SO-100231.md');
  const done = await stack.call('promote_draft', { draft_id: mine.draft_id });
  expect(done.ok, JSON.stringify(done)).toBe(true);

  // NOW the database has the change (one row), the other rows are as they were, and the page says so.
  expect(orderInDb(stack.home, 'SO-100231')).toEqual({ status: 'shipped', qty: 40 });
  expect(orderInDb(stack.home, 'SO-100232').status).toBe('open');
  await openRow(page, 'orders-db', /SO-100232/);
  await openRow(page, 'orders-db', /SO-100231/);
  const again = await webviewWith(page, 'escurel-page-as-ui', 'SO-100231');
  await expect(again.locator('.field[data-name="status"]')).toContainText('shipped', {
    timeout: 20_000,
  });
  await expect(again.locator('.write-back')).toContainText('applied');
  await stack.shot('15-sql-write-back-applied');
});

// The journeys, in every direction: from the Inbox, from Awaiting You, from Knowledge and from the
// thread itself, a person can reach the skill, the page, the run and the thread of whatever they are
// looking at. Each step asserts the destination, not just that a link exists.

async function menuItems(page: Page): Promise<string[]> {
  const items = page.locator('.monaco-menu .action-item .action-label');
  await expect(items.first()).toBeVisible();
  return (await items.allInnerTexts()).map((t) => t.trim()).filter(Boolean);
}
const tabNames = (page: Page) => page.locator('.tab .label-name').allInnerTexts();

test('journey: from the Inbox to the thread, the skill and the records the run proposed', async ({
  stack,
}) => {
  const { page } = stack;
  const rows = pane(page, 'Inbox').getByRole('treeitem', { name: /order-4500131/ });
  await rows.first().click({ button: 'right' });
  const menu = await menuItems(page);
  expect(menu).toContain('Open thread');
  expect(menu).toContain('View skill');
  await stack.shot('journeys/J1-inbox-menu');
  await page.keyboard.press('Escape');
  // Earlier scenarios may have filed more signals about this order (a plan waiting for approval, which
  // has proposed nothing yet). Open them in turn and keep the thread whose run proposed records.
  let wv: FrameLocator | undefined;
  for (let i = 0; i < (await rows.count()); i += 1) {
    await rows.nth(i).click();
    // While VS Code switches tabs the outgoing webview still covers the incoming one for a moment:
    // resolve the frame afresh on every try.
    await expect(async () => {
      wv = await webviewWith(page, 'escurel-thread-canvas', 'order-4500131');
      await wv.getByRole('button', { name: 'Fit' }).click({ timeout: 3_000 });
    }).toPass({ timeout: 20_000 });
    const pages = await wv
      .locator('escurel-thread-canvas .card.type-page')
      .count()
      .catch(() => 0);
    if (pages >= 2) break;
  }
  if (!wv) throw new Error('no Inbox row for order-4500131 opened a thread');
  const canvas = wv.locator('escurel-thread-canvas');
  // The records the run proposed are cards of the thread, and each says which skill's record it is:
  // an order is not an analysis.
  const order = canvas.locator('.card.type-page').filter({ hasText: 'order-4500131' });
  const analysis = canvas.locator('.card.type-page').filter({ hasText: 'supplier-risk-analysis' });
  await expect(order.locator('.type-qualifier')).toHaveText('customer-order');
  await expect(analysis.locator('.type-qualifier')).toHaveText('supplier-risk-analysis');
  await expect(canvas.locator('.card.type-run .type-qualifier')).toContainText('on order-4500131');
  await stack.shot('journeys/J2-cards-name-their-skill');
  // Select the analysis card: the details panel offers its skill and the run behind it.
  await analysis.click({ position: { x: 12, y: 8 } });
  const details = await webviewWith(page, 'escurel-details');
  await expect(
    details.getByRole('button', { name: 'View skill: supplier-risk-analysis' }),
  ).toBeVisible();
  await expect(details.getByRole('button', { name: 'Open the run behind it' })).toBeVisible();
  await stack.shot('journeys/J3-details-links');
  await details.getByRole('button', { name: 'View skill: supplier-risk-analysis' }).click();
  await expect(page.locator('.tab.active .label-name')).toContainText('supplier-risk-analysis');
});

test('journey: from a record back to the thread and run that wrote it, and to its report', async ({
  stack,
}) => {
  const { page } = stack;
  // The analysis of the story's FIRST thread was promoted: its page exists.
  await openRow(page, 'supplier-risk-analysis', /^meier-guss-\d{4}-\d{2}-\d{2}/);
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  const el = wv.locator('escurel-page-as-ui');
  // It carries the thread strip even though no run-finished row sits on this page: the review rows of
  // the promoted draft name the run.
  await expect(el.locator('.thread-strip .open-thread')).toBeVisible();
  await expect(el.locator('.thread-strip .open-run')).toBeVisible();
  // And it says which report draws it: a report is a definition, never a node of a thread.
  await expect(el.locator('.viewer')).toContainText('Chart: supplier-risk-report');
  await expect(el.locator('.viewer')).toContainText('Peacock');
  await stack.shot('journeys/J4-record-provenance-and-report');
  await el.locator('.thread-strip .open-run').click();
  // The run opens under a name a person recognises: the skill and the page, not an id.
  await expect(page.locator('.tab.active .label-name')).toContainText(/Run · supplier-risk · /);
  const run = await webviewWith(page, 'escurel-run-detail');
  await expect(run.locator('escurel-run-detail h1')).toContainText('supplier-risk on ');
  await expect(run.getByRole('button', { name: 'View skill: supplier-risk' })).toBeVisible();
  await expect(run.getByRole('button', { name: 'Open thread' })).toBeVisible();
  await stack.shot('journeys/J5-run-detail-links');
  await run.getByRole('button', { name: 'Open thread' }).click();
  await expect(page.locator('.tab.active .label-name')).toContainText('Thread ·');
});

test('journey: Knowledge and Awaiting You rows lead to threads, runs and skills', async ({
  stack,
}) => {
  const { page } = stack;
  const aw = pane(page, 'Awaiting You').getByRole('treeitem').first();
  await aw.click({ button: 'right' });
  const awaitingMenu = await menuItems(page);
  for (const want of ['Open thread', 'Open run', 'View skill', 'Promote', 'Discard']) {
    expect(awaitingMenu).toContain(want);
  }
  await page.keyboard.press('Escape');

  // A skill leads to the threads it started.
  const skill = await skillRow(page, 'supplier-risk');
  await skill.click({ button: 'right' });
  expect(await menuItems(page)).toContain('Show threads using this skill');
  await chooseMenuItem(page, 'Show threads using this skill');
  const picker = page.locator('.quick-input-widget');
  await expect(picker).toBeVisible();
  await expect(picker.locator('.quick-input-list .monaco-list-row').first()).toBeVisible();
  await stack.shot('journeys/J6-threads-of-a-skill');
  const before = await tabNames(page);
  await picker.locator('.quick-input-list .monaco-list-row').first().click();
  await expect.poll(async () => (await tabNames(page)).length >= before.length).toBe(true);

  // A report is never run: say so, instead of an empty list.
  const report = await skillRow(page, 'supplier-risk-report');
  await report.click({ button: 'right' });
  await chooseMenuItem(page, 'Show threads using this skill');
  await expect(page.locator('.notification-toast', { hasText: 'is a report' })).toBeVisible();
  await stack.shot('journeys/J7-a-report-is-not-run');

  // A record leads to the thread and the run that wrote it.
  const instance = await knowledgeRow(page, /^meier-guss-\d{4}-\d{2}-\d{2}/);
  await instance.click({ button: 'right' });
  const instanceMenu = await menuItems(page);
  expect(instanceMenu).toContain('Open thread');
  expect(instanceMenu).toContain('Open run');
  await page.keyboard.press('Escape');
});

test('a skill opens as a readable page, and Show Markdown opens its source', async ({ stack }) => {
  const { page } = stack;
  const skill = await skillRow(page, 'supplier-risk');
  await skill.click({ button: 'right' });
  await chooseMenuItem(page, 'View skill');
  const wv = await webviewWith(page, 'escurel-skill-page');
  await expect(wv.locator('escurel-skill-page h1')).toContainText(/supplier/i);
  for (const heading of ['About', 'Fields', 'Records', 'Recent runs']) {
    await expect(wv.getByRole('heading', { name: new RegExp(`^${heading}`) })).toBeVisible();
  }
  await stack.shot('09-skill-page');
  await wv.getByRole('button', { name: 'Show Markdown' }).click();
  await expect(page.locator('.tab.active', { hasText: 'supplier-risk.md' })).toBeVisible();
  await expect(page.locator('.view-lines').first()).toContainText('kind:');
  await stack.shot('09b-skill-markdown');
});

test('Explain this view tells how events, skills, runs and records connect', async ({ stack }) => {
  const { page } = stack;
  await page.keyboard.press('F1');
  await page.keyboard.type('Escurel: Explain this view');
  await page.keyboard.press('Enter');
  const wv = await webviewWith(page, 'h1');
  await expect(wv.getByRole('heading', { name: /How things connect/ })).toBeVisible();
  await expect(wv.getByText('Awaiting You').first()).toBeVisible();
  await stack.shot('10-explain-this-view');
});

test('nothing in the extension threw while all of that happened', async ({ stack }) => {
  const ours = stack.errors.filter((e) => /escurel|pageerror/i.test(e));
  expect(ours).toEqual([]);
});
