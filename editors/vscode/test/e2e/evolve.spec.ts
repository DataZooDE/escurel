import type { Page } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { join } from 'node:path';
import { expect, test, webviewWith } from './fixtures';

const pane = (page: Page, title: string) =>
  page.locator('.pane', { has: page.locator('.pane-header', { hasText: title }) });

type GatewayEvent = {
  event_id: string;
  instance_page_id?: string;
  provenance?: { manual?: Record<string, unknown> };
  revision_binding_attested?: boolean;
};

async function events(
  call: (name: string, args: Record<string, unknown>) => Promise<Record<string, unknown>>,
  label: string,
): Promise<GatewayEvent[]> {
  const result = await call('list_events', { label_skill: label, limit: 100 });
  return (result.events ?? []) as GatewayEvent[];
}

test('a person reviews Evolve search limits before approving the frozen plan', async ({ stack }) => {
  const id = `v2-visible-approval-${Date.now()}`;
  const pageId = `markdown/instances/evolve_problem/${id}.md`;
  const spec = {
    pilot: 'p1_decision', version: 2, holdout_id: 'registered-private-holdout',
    max_generations: 2, budget: { max_evaluated: 8, max_usd: 3.5 },
    service_targets: { min_fill_rate: 0.95, min_sku_fill_rate: 0.9 },
    source_sha256: 'a'.repeat(64), seed_sql: 'SELECT 1 AS order_qty',
    baseline_sql: 'SELECT 1 AS order_qty',
  };
  const content = `---\ntype: instance\nskill: evolve_problem\nid: ${id}\nowner_subject: alice\npilot: p1_decision\nsearch_request: ${JSON.stringify(spec)}\n---\n# Visible approval review\n`;
  const written = await stack.call('update_page', {
    page_id: pageId, content, base_sha256: '',
  });
  expect(written.ok).toBe(true);
  const expanded = await stack.call('expand', { page_id: pageId, raw: true });
  const revision = expanded.content_sha256 as string;
  expect(revision).toMatch(/^[0-9a-f]{64}$/);

  const knowledge = pane(stack.page, 'Knowledge');
  const folder = knowledge.getByRole('treeitem', { name: /^evolve_problem/ });
  await expect(folder).toBeVisible({ timeout: 30_000 });
  await folder.click();
  await knowledge.getByRole('treeitem', { name: new RegExp(id) }).click();
  const pageUi = await webviewWith(stack.page, 'escurel-page-as-ui');
  await pageUi.getByRole('button', { name: 'Review experiment plan', exact: true }).click();

  let preflight: GatewayEvent | undefined;
  await expect.poll(async () => {
    preflight = (await events(stack.call, 'evolve_preflight'))
      .find((event) => event.instance_page_id === pageId);
    return preflight?.event_id;
  }, { timeout: 30_000 }).toBeTruthy();
  expect(preflight!.provenance?.manual?.expected_page_sha256).toBe(revision);
  expect((await events(stack.call, 'evolve_run')).some((event) => event.instance_page_id === pageId))
    .toBe(false);

  await stack.call('capture_event', {
    event_id: `evolve-visible-preflight-final-${id}`,
    label_skill: 'evolve:preflight', source: 'anofox-evolve', mime: 'application/json',
    kind: 'system', instance_page_id: '', title: 'problem-structure-checked',
    body: JSON.stringify({ problem_sha256: revision, structural_ready_for_start: true,
      evidence_scope: 'structure_static_sql_and_holdout_binding_only' }),
    provenance: { runner: { root_event_id: preflight!.event_id },
      evolve: { phase: 'final', problem_sha256: revision, ready: true } },
  }, true);

  let planned: GatewayEvent | undefined;
  await expect.poll(async () => {
    planned = (await events(stack.call, 'evolve_run'))
      .find((event) => event.instance_page_id === pageId);
    return planned?.event_id;
  }, { timeout: 30_000 }).toBeTruthy();
  expect(planned!.provenance?.manual?.target_page_sha256).toBe(revision);
  expect(planned!.revision_binding_attested).toBe(true);

  let runId: string | undefined;
  await expect.poll(async () => {
    const lineage = await stack.call('list_lineage', { root_event_id: planned!.event_id });
    const node = (lineage.nodes as Array<{ type: string; state: string; id: string }> | undefined)
      ?.find((item) => item.type === 'run' && item.state === 'planned');
    runId = node?.id;
    return runId;
  }, { timeout: 45_000 }).toBeTruthy();

  const thread = await webviewWith(stack.page, 'escurel-thread-canvas');
  await thread.locator('escurel-thread-canvas .card.type-run').first().click();
  await thread.getByRole('button', { name: 'Approve plan' }).click();
  // VS Code uses a native Electron dialog for modal messages. CDP only sees the
  // workbench renderer, so inspect and click the dialog on this isolated X11 display.
  const nativeDialog = execFileSync('python3', ['-c', `
from Xlib import display, X
from Xlib.ext import xtest
from PIL import Image
import sys
import time
d = display.Display()
r = d.screen().root
window = None
for _ in range(100):
    window = next((c for c in r.query_tree().children if c.get_wm_name() == 'Visual Studio Code'), None)
    if window: break
    time.sleep(0.1)
if window is None: raise RuntimeError('VS Code approval dialog did not appear')
dialog = window.get_geometry()
if dialog.width < 500 or dialog.height < 250: raise RuntimeError('approval dialog is unexpectedly small')
g = r.get_geometry()
raw = r.get_image(0, 0, g.width, g.height, X.ZPixmap, 0xffffffff)
Image.frombytes('RGB', (g.width, g.height), raw.data, 'raw', 'BGRX').save(sys.argv[1])
px = dialog.x + int(dialog.width * 0.75)
py = dialog.y + dialog.height - 25
xtest.fake_input(d, X.MotionNotify, x=px, y=py)
xtest.fake_input(d, X.ButtonPress, detail=1)
xtest.fake_input(d, X.ButtonRelease, detail=1)
d.sync()
print(f'{dialog.width}x{dialog.height}')
`, join(__dirname, 'artifacts', 'evolve-approval-limits.png')], {
    encoding: 'utf8', env: { ...process.env, DISPLAY: stack.display },
  }).trim();
  expect(nativeDialog).toMatch(/\d+x\d+$/);

  const approvalId = `evolve-approval-${runId}`;
  let approval: GatewayEvent | undefined;
  await expect.poll(async () => {
    approval = (await events(stack.call, 'evolve_run'))
      .find((event) => event.event_id === approvalId);
    return approval?.event_id;
  }, { timeout: 30_000 }).toBe(approvalId);
  expect(approval!.revision_binding_attested).toBe(true);
  expect(approval!.provenance?.manual?.expected_page_sha256).toBe(revision);
  expect(approval!.provenance?.manual?.approved_plan_run_id).toBe(runId);
});
