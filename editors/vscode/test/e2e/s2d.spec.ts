import { expect, test, webviewWith } from './fixtures';
import { knowledgeRow, openRow, pane, skillRow } from './helpers';

// The Source-to-Deliver demo (the hetzner seed) in a real window: three agent proposals wait for a
// planner. Serial, one window, screenshots for a person to look at.
test.use({ s2d: true });
test.describe.configure({ mode: 'serial' });

test('S2D: the supplier exception record shows its impact figures', async ({ stack }) => {
  const { page } = stack;
  await expect(await knowledgeRow(page, /^folder logistics\/source$/)).toBeVisible();
  await stack.shot('s2d-01-knowledge');
  await openRow(page, 'supplier_exception', /l-24117/);
  const wv = await webviewWith(page, 'escurel-page-as-ui');
  await expect(wv.locator('.report .kpi').first()).toBeVisible();
  await stack.shot('s2d-02-exception-page');
});
