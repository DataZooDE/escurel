import { expect, test, webviewWith } from './fixtures';
import { openRow } from './helpers';

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

const syntheticSpec = {
    pilot: 'p1_decision', version: 2, evaluator_version: 'replenishment_decision_v2', holdout_id: 'registered-private-holdout',
    max_generations: 2, budget: { max_evaluated: 8, max_usd: 3.5 },
    capacity: 10,
    skus: [{ sku_id: 1, name: 'Synthetic training SKU', initial_stock: 1,
      initial_pipeline: [0, 0, 0, 0, 0, 0], history: [1, 1], demand: [1, 1, 1, 1, 1, 2],
      lead_time: 1, case_pack: 1, min_order: 0, holding_cost: 0,
      shortage_cost: 10, fixed_order_cost: 5 }],
    service_targets: { aggregate_min_fill_rate: 0.8, per_sku_min_fill_rate: { '1': 0.8 } },
    seed_sql: 'SELECT sku_id, 1::BIGINT AS order_qty FROM p1_observation',
    baseline_sql: 'SELECT sku_id, 1::BIGINT AS order_qty FROM p1_observation',
    planning_window_days: 2, scored_window_days: 2,
    unit_order_costs: { '1': 1 }, terminal_stock_tolerance: { '1': 0 },
    training_start: '2026-08-01', training_end: '2026-08-06',
    history_start: '2026-07-30', history_end: '2026-07-31',
    training_source_id: 'synthetic-training-source', source_sha256: 'a'.repeat(64),
};

test.use({ runnerHarness: 'gemini' });

test('owner reviews a real runner plan in the native window', async ({ stack }) => {
  const id = `v2-gemini-plan-${Date.now()}`;
  const pageId = `markdown/instances/evolve_problem/${id}.md`;
  const content = `---\nkind: instance\nskill: evolve_problem\nid: ${id}\nowner_subject: alice\npilot: p1_decision\nsearch_request: ${JSON.stringify(syntheticSpec)}\n---\n# Native runner plan review\n`;
  const written = await stack.call('update_page', {
    page_id: pageId, content, base_sha256: '',
  });
  expect(written.ok).toBe(true);
  const revision = (await stack.call('expand', { page_id: pageId, raw: true })).content_sha256 as string;
  expect(revision).toMatch(/^[0-9a-f]{64}$/);
  stack.setGeminiPlanTarget(pageId, revision);

  await openRow(stack.page, 'evolve_problem', new RegExp(id));
  const pageUi = await webviewWith(stack.page, 'escurel-page-as-ui');
  await pageUi.getByRole('button', { name: 'Review experiment plan', exact: true }).click();

  let preflight: GatewayEvent | undefined;
  await expect.poll(async () => {
    preflight = (await events(stack.call, 'evolve_preflight'))
      .find((event) => event.instance_page_id === pageId);
    return preflight?.event_id;
  }, { timeout: 30_000 }).toBeTruthy();
  expect(preflight!.provenance?.manual?.expected_page_sha256).toBe(revision);
  // This fixture supplies structural readiness; the separate Rust gate
  // executes Evolve's real preflight and holdout-binding checks.
  await stack.call('capture_event', {
    event_id: `evolve-gemini-preflight-final-${id}`,
    label_skill: 'evolve:preflight', source: 'anofox-evolve', mime: 'application/json',
    kind: 'system', instance_page_id: '', title: 'problem-structure-checked',
    body: JSON.stringify({ problem_sha256: revision, structural_ready_for_start: true,
      holdout_contract: {
        holdout_sha256: 'b'.repeat(64), declared_holdout_source_ref: 'synthetic-fixture',
        declared_holdout_source_sha256: 'c'.repeat(64),
        training_source_id: 'synthetic-training-source', training_source_sha256: 'a'.repeat(64),
        training_start: '2026-08-01', training_end: '2026-08-06',
        holdout_start: '2026-09-01', holdout_end: '2026-09-06',
        sku_count: 1, evaluator_version: 'replenishment_decision_v2',
        service_targets: syntheticSpec.service_targets, baseline_sql_sha256: 'd'.repeat(64),
        max_cost_ratio: 1, sensitivity_tail_days: [1, 4],
      },
    }),
    provenance: { runner: { root_event_id: preflight!.event_id },
      evolve: { phase: 'final', problem_sha256: revision, ready: true } },
  }, true);
  const contractDialog = stack.page.getByRole('dialog', { name: 'Info' })
    .filter({ hasText: 'Review the frozen private holdout contract' });
  await expect(contractDialog).toContainText('synthetic-fixture');
  await expect(contractDialog).toContainText('2026-09-01 to 2026-09-06');
  await contractDialog.getByRole('button', { name: 'Review experiment plan' }).click();

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
  const runRows = (await stack.call('list_events', { run_id: runId, include_system: true }))
    .events as Array<{ title: string; body: string; provenance?: { runner?: { harness?: string } } }>;
  expect(runRows.some((row) => row.title === 'run-progress')).toBe(true);
  const finished = runRows.find((row) => row.title === 'run-finished');
  expect(finished?.provenance?.runner?.harness).toBe('gemini');
  expect(JSON.parse(finished!.body).plan).toHaveLength(2);
  expect(stack.geminiRequests).toHaveLength(3);
  const firstRequest = JSON.stringify(stack.geminiRequests[0]);
  expect(firstRequest).toContain(pageId);
  expect(firstRequest).toContain(revision);
  const reviewedRequest = JSON.stringify(stack.geminiRequests[1]);
  expect(reviewedRequest).toContain('functionResponse');
  expect(reviewedRequest).toContain('expand');
  for (const required of [revision, 'registered-private-holdout',
    'synthetic-training-source', 'replenishment_decision_v2', 'max_evaluated']) {
    expect(reviewedRequest).toContain(required);
  }

  const thread = await webviewWith(stack.page, 'escurel-thread-canvas');
  const card = thread.locator(`escurel-thread-canvas .card.type-run[data-node-id="${runId}"]`);
  await expect(card).toBeVisible();
  await card.click();
  await thread.getByRole('button', { name: 'Approve plan' }).click();
  const dialog = stack.page.getByRole('dialog', { name: 'Warning' })
    .filter({ hasText: 'Approve this Evolve search' });
  await expect(dialog).toContainText('Plan harness: gemini');
  await expect(dialog).toContainText('Review the frozen source, holdout and V2 budget');
  await expect(dialog).toContainText('8 evaluations; 3.50 USD max');
  await dialog.getByRole('button', { name: 'Approve search' }).click();

  await expect.poll(async () => (await events(stack.call, 'evolve_run'))
    .find((event) => event.event_id === `evolve-approval-${runId}`)?.event_id,
  { timeout: 30_000 }).toBe(`evolve-approval-${runId}`);
  const approval = (await events(stack.call, 'evolve_run'))
    .find((event) => event.event_id === `evolve-approval-${runId}`);
  expect(approval?.provenance?.manual?.harness).toBe('gemini');
  expect(approval?.provenance?.manual?.approved_plan_run_id).toBe(runId);
  expect(approval?.provenance?.manual?.expected_page_sha256).toBe(revision);
  expect(approval?.revision_binding_attested).toBe(true);

  // This isolated VS Code fixture has no Evolve service. Supply its receipt and
  // page projection to verify the owner's visible navigation after approval.
  const experimentPageId = `markdown/instances/evolve_experiment/${id}.md`;
  const experimentPage = `---\nkind: instance\nskill: evolve_experiment\nid: ${id}\nowner_subject: alice\nstatus: running\nevidence_scope: synthetic_ui_fixture\n---\n# Synthetic experiment ${id}\n`;
  const projected = await stack.call('update_page', {
    page_id: experimentPageId, content: experimentPage, base_sha256: '',
  }, true);
  expect(projected.ok).toBe(true);
  const receiptId = `synthetic-admission-${id}`;
  await stack.call('capture_event', {
    event_id: receiptId, label_skill: 'evolve:admission', source: 'anofox-evolve',
    mime: 'text/markdown', kind: 'system', instance_page_id: '',
    title: 'experiment-admitted',
    body: `Experiment accepted: [[evolve_experiment::${id}]].`,
    provenance: { runner: { root_event_id: `evolve-approval-${runId}` },
      evolve: { approval_event_id: `evolve-approval-${runId}`, experiment_id: id,
        problem_sha256: revision } },
  }, true);
  const approvalThread = await webviewWith(stack.page, 'escurel-thread-canvas');
  const receiptCard = approvalThread.locator(`escurel-thread-canvas .card[data-node-id="${receiptId}"]`);
  await expect(receiptCard).toBeVisible();
  await receiptCard.click();
  const approvalDetails = await webviewWith(stack.page, 'escurel-details');
  const experimentLink = approvalDetails.locator('escurel-thread-inspector .wikilink');
  await expect(experimentLink).toContainText(id);
  await experimentLink.click();
  await expect(stack.page.getByRole('tab', { name: new RegExp(id), selected: true })).toBeVisible();
  const experimentUi = await webviewWith(stack.page, 'escurel-page-as-ui');
  await expect(experimentUi.getByText('Synthetic experiment ' + id)).toBeVisible();
  await expect(experimentUi.getByText('running', { exact: true })).toBeVisible();
});
