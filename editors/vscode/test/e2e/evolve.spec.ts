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

test('echo plan is refused and a synthetic non-echo plan approves the exact revision', async ({ stack }) => {
  const id = `v2-visible-approval-${Date.now()}`;
  const pageId = `markdown/instances/evolve_problem/${id}.md`;
  const spec = syntheticSpec;
  const content = `---\nkind: instance\nskill: evolve_problem\nid: ${id}\nowner_subject: alice\npilot: p1_decision\nsearch_request: ${JSON.stringify(spec)}\n---\n# Visible approval review\n`;
  const written = await stack.call('update_page', {
    page_id: pageId, content, base_sha256: '',
  });
  expect(written.ok).toBe(true);
  const expanded = await stack.call('expand', { page_id: pageId, raw: true });
  const revision = expanded.content_sha256 as string;
  expect(revision).toMatch(/^[0-9a-f]{64}$/);

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
  expect((await events(stack.call, 'evolve_run')).some((event) => event.instance_page_id === pageId))
    .toBe(false);

  // The UI test injects a ready receipt. The Rust patched-gateway test uses
  // Evolve's actual preflight and holdout registry before approving a search.
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
  await expect(stack.page.getByRole('alert').filter({ hasText: 'Echo or unlabelled plans cannot authorize Evolve search' }))
    .toBeVisible();
  expect((await events(stack.call, 'evolve_run'))
    .some((event) => event.event_id === `evolve-approval-${runId}`)).toBe(false);

  // Inject a representative completed non-echo plan into the native window.
  // The Rust gateway test exercises the real Evolve preflight and admission;
  // this checks the visible approval, frozen revision, and captured user event.
  const reviewedRunId = `v2-reviewed-plan-${id}`;
  for (const [suffix, title, body] of [
    ['started', 'run-started', {}],
    ['finished', 'run-finished', { status: 'planned', plan: [
      { step: 'Verify the sealed holdout and source binding', status: 'pending' },
      { step: 'Run bounded DuckDB evaluations under the approved budget', status: 'pending' },
    ] }],
  ] as const) {
    await stack.call('capture_event', {
      event_id: `run:${reviewedRunId}:${suffix}`,
      label_skill: 'escurel:run', source: 'escurel-runner', kind: 'system',
      instance_page_id: pageId, title, body: JSON.stringify(body),
      provenance: { runner: { run_id: reviewedRunId, harness: 'codex',
        event_id: planned!.event_id, root_event_id: planned!.event_id,
        target_page_id: pageId,
        manual: planned!.provenance?.manual } },
    }, true);
  }
  await expect.poll(async () => {
    const lineage = await stack.call('list_lineage', { root_event_id: planned!.event_id });
    return (lineage.nodes as Array<{ type: string; state: string; id: string }> | undefined)
      ?.some((item) => item.type === 'run' && item.id === reviewedRunId && item.state === 'planned');
  }, { timeout: 30_000 }).toBe(true);

  const reviewedCard = thread.locator(`escurel-thread-canvas .card.type-run[data-node-id="${reviewedRunId}"]`);
  await expect(reviewedCard).toBeVisible({ timeout: 30_000 });
  await reviewedCard.dblclick();
  const run = await webviewWith(stack.page, 'escurel-run-detail');
  await expect(run.getByRole('button', { name: 'Review search limits' })).toBeVisible();
  await run.getByRole('button', { name: 'Review search limits' }).click();
  const dialog = stack.page.getByRole('dialog', { name: 'Warning' })
    .filter({ hasText: 'Approve this Evolve search' });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText('Plan harness: codex');
  await expect(dialog).toContainText('Verify the sealed holdout and source binding');
  await expect(dialog).toContainText('8 evaluations; 3.50 USD max');
  await expect(dialog).toContainText('replenishment_decision_v2; planning window: 2 days; scored window: 2 days');
  await expect(dialog).toContainText('Unit order costs by SKU: {"1":1}');
  await expect(dialog).toContainText('Terminal stock tolerance by SKU: {"1":0}');
  await expect(dialog).toContainText('Training source ID: synthetic-training-source');
  await dialog.getByRole('button', { name: 'Approve search' }).click();

  await expect.poll(async () => (await events(stack.call, 'evolve_run'))
    .find((event) => event.event_id === `evolve-approval-${reviewedRunId}`)?.event_id,
  { timeout: 30_000 }).toBe(`evolve-approval-${reviewedRunId}`);
  const approval = (await events(stack.call, 'evolve_run'))
    .find((event) => event.event_id === `evolve-approval-${reviewedRunId}`);
  expect(approval?.provenance?.manual?.approved_plan_run_id).toBe(reviewedRunId);
  expect(approval?.provenance?.manual?.harness).toBe('codex');
  expect(approval?.provenance?.manual?.expected_page_sha256).toBe(revision);
  expect(approval?.revision_binding_attested).toBe(true);
});
