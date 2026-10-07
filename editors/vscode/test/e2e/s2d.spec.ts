import { expect, test, webviewWith, type Stack } from './fixtures';
import { knowledgeRow, openRow, pane } from './helpers';

// The Source-to-Deliver demo (the hetzner seed) in a real window: three agent proposals wait for a
// planner. One window, played in order, with the numbers of the rehearsal script asserted and a
// screenshot per step for a person to look at (demo/s2d/REHEARSAL.md is the same script).
test.use({ s2d: true });
test.describe.configure({ mode: 'serial' });

type Page = { frontmatter?: Record<string, unknown>; page?: unknown };
const expandPage = async (stack: Stack, skill: string, id: string) =>
  (await stack.call('expand', { page_id: `markdown/instances/${skill}/${id}.md` })) as Page;

/** Approve what is waiting for the planner: open its row in Awaiting you and apply all of it. */
async function approve(stack: Stack, row: RegExp) {
  const { page } = stack;
  const item = pane(page, 'Awaiting You').getByRole('treeitem', { name: row }).first();
  await item.hover();
  await item
    .getByRole('button', { name: /Promote/ })
    .first()
    .click();
}

test('S2D: the supplier mail is in the inbox and its record shows the impact', async ({
  stack,
}) => {
  const { page } = stack;
  await expect(
    pane(page, 'Inbox').getByRole('treeitem', { name: /Delivery delay PO-4500182/ }),
  ).toBeVisible();
  await expect(await knowledgeRow(page, /^folder logistics\/source$/)).toBeVisible();
  await stack.shot('s2d-01-knowledge');
  await openRow(page, 'supplier_exception', /l-24117/);
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  const kpis = wv.locator('.report .kpi');
  await expect(kpis).toHaveCount(2);
  await expect(kpis.nth(0)).toContainText('4');
  await expect(kpis.nth(0)).toContainText('Orders late');
  await expect(kpis.nth(1)).toContainText('132,400');
  // The orders that go late, worst first: 18, 13, 9 and 4 days.
  const days = await wv
    .locator('.report tbody tr')
    .evaluateAll((rows) =>
      rows.map((r) => Array.from(r.querySelectorAll('td')).map((c) => c.textContent?.trim())),
    );
  expect(days.length).toBe(12);
  await stack.shot('s2d-02-exception-impact');
});

test('S2D: the proposal can be read before it is approved, tables and all', async ({ stack }) => {
  const { page } = stack;
  const awaiting = pane(page, 'Awaiting You');
  await awaiting
    .getByRole('treeitem', { name: /tp-stuttgart-lyon/ })
    .first()
    .click();
  const picker = page.locator('.quick-input-widget');
  await expect(picker).toBeVisible();
  await stack.shot('s2d-03a-review-picker');
  // The proposal itself: the draft opens as the page it would create.
  await picker.getByRole('option', { name: /tp-stuttgart-lyon-fr-2026-10-08.*new page/ }).click();
  await expect(page.locator('.monaco-editor, .monaco-diff-editor').first()).toBeVisible();
  await stack.shot('s2d-03b-review-before-approval');
  // The review editor offers a rendered preview: the proposal as the page it would create, tables and all.
  await page.getByRole('button', { name: 'Preview the proposal' }).first().click();
  const preview = await webviewWith(page, 'h1', 'Consolidation: Stuttgart to Lyon');
  await expect(
    preview.getByRole('heading', { name: /Consolidation: Stuttgart to Lyon/ }),
  ).toBeVisible();
  await expect(preview.getByRole('cell', { name: 'SH-77003' }).last()).toBeVisible();
  await stack.shot('s2d-03c-review-preview');
  // Leave the window as the next scenario expects it: the preview and the draft closed.
  await page.keyboard.press('Control+w');
  await page.keyboard.press('Control+w');
});

test('S2D: approving the proposal records the decision and resolves the exception', async ({
  stack,
}) => {
  const { page } = stack;
  // Awaiting you holds three proposals from the three agents; the supplier one is the first story.
  await expect(
    pane(page, 'Awaiting You').getByRole('treeitem', { name: /res-l-24117/ }),
  ).toBeVisible();
  await expect(
    pane(page, 'Awaiting You').getByRole('treeitem', { name: /tp-stuttgart-lyon/ }),
  ).toBeVisible();
  await expect(
    pane(page, 'Awaiting You').getByRole('treeitem', { name: /ltb-sp-3307/ }),
  ).toBeVisible();
  await stack.shot('s2d-03-awaiting');
  await approve(stack, /res-l-24117/);
  await stack.shot('s2d-04-after-approve');
  // Both pages move together: the decision exists as approved, the exception is resolved.
  await expect
    .poll(
      async () =>
        (await expandPage(stack, 'exception_resolution', 'res-l-24117')).frontmatter?.status,
    )
    .toBe('approved');
  expect((await expandPage(stack, 'supplier_exception', 'l-24117')).frontmatter?.status).toBe(
    'resolved',
  );
  await expect(
    pane(page, 'Awaiting You').getByRole('treeitem', { name: /res-l-24117/ }),
  ).toHaveCount(0);
  // The open record follows: it was waiting for the planner and now shows the exception resolved.
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  await expect(wv.locator('.field[data-name="status"] .value')).toHaveText(/resolved/);
  await stack.shot('s2d-05-approved');
});

test("S2D: the approved resolution shows the optimizer's cheapest plan", async ({ stack }) => {
  const { page } = stack;
  await openRow(page, 'exception_resolution', /res-l-24117/);
  const wv = await webviewWith(page, 'escurel-page-as-ui', 'res-l-24117');
  const report = wv.locator('.report');
  await expect(report).toContainText('partial-expedite');
  await expect(report).toContainText('realloc-wh-mid');
  // Solved exactly, not guessed: the page says how the plan was found.
  await expect(report).toContainText('opt_knapsack_exact');
  await stack.shot('s2d-05b-recovery-plan');
});

test('S2D: the transport proposal consolidates three shipments and keeps the one with a duty', async ({
  stack,
}) => {
  const { page } = stack;
  await approve(stack, /tp-stuttgart-lyon/);
  await expect
    .poll(
      async () =>
        (await expandPage(stack, 'transport_plan', 'tp-stuttgart-lyon-fr-2026-10-08')).frontmatter
          ?.status,
    )
    .toBe('approved');
  const fm = (await expandPage(stack, 'transport_plan', 'tp-stuttgart-lyon-fr-2026-10-08'))
    .frontmatter!;
  expect(fm.shipments).toEqual(['SH-77001', 'SH-77002', 'SH-77003']);
  expect(fm.pallets).toBe(23);
  expect(fm.held_pallets).toBe(14);
  await openRow(page, 'transport_plan', /tp-stuttgart-lyon/);
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  const rows = wv.locator('.report tbody tr');
  await expect(rows.first()).toBeVisible();
  const text = (await rows.allTextContents()).join('\n');
  // The three that ship together on Thursday, the one whose delivery duty is 2026-10-09 stays.
  for (const id of ['SH-77001', 'SH-77002', 'SH-77003', 'SH-77004', 'SH-77005'])
    expect(text).toContain(id);
  expect(text).toContain('would miss delivery duty 2026-10-09');
  // Packed into trucks by the optimizer: one truck.
  await expect(wv.locator('.report')).toContainText('opt_pack_best_of');
  await stack.shot('s2d-06-transport-plan');
});

test('S2D: the last-time-buy proposal holds the service level with 640 units', async ({
  stack,
}) => {
  const { page } = stack;
  await approve(stack, /ltb-sp-3307/);
  await expect
    .poll(async () => (await expandPage(stack, 'ltb_decision', 'ltb-sp-3307')).frontmatter?.status)
    .toBe('approved');
  const fm = (await expandPage(stack, 'ltb_decision', 'ltb-sp-3307')).frontmatter!;
  expect(fm.qty).toBe(640);
  expect(fm.part).toBe('SP-3307');
  await openRow(page, 'ltb_decision', /ltb-sp-3307/);
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  await expect(wv.locator('.report .kpi').first()).toContainText('96%');
  await expect(wv.locator('.report .kpi').first()).toContainText('Probability it lasts');
  const body = await wv.locator('.report').innerText();
  // The warehouse split of the 640 units.
  for (const n of ['352', '160', '128']) expect(body).toContain(n);
  await stack.shot('s2d-07-last-time-buy');
});
