import type { Page } from '@playwright/test';
import { expect, test, webviewWith } from './fixtures';

test.use({ runnerHarness: 'gemini', evolveAgentBin: process.env.EVOLVE_AGENT_BIN });
test.skip(!process.env.EVOLVE_AGENT_BIN, 'Set EVOLVE_AGENT_BIN to test the joined Evolve service');

const pane = (page: Page, title: string) =>
  page.locator('.pane', { has: page.locator('.pane-header', { hasText: title }) });

type EventRow = {
  event_id: string;
  title: string;
  body: string;
  kind?: string;
  source?: string;
  root_event_id?: string;
  instance_page_id?: string;
  provenance?: { manual?: Record<string, unknown>; evolve?: Record<string, unknown> };
};

test('native owner approval, seed run, and validation through Evolve', async ({ stack }) => {
  const id = `native-service-${Date.now()}`;
  const baseline = 'SELECT sku_id, 1::BIGINT AS order_qty FROM p1_observation';
  const seed = 'SELECT sku_id, CASE WHEN period % 3 = 0 THEN 2 WHEN period % 3 = 2 THEN 1 ELSE 0 END::BIGINT AS order_qty FROM p1_observation';
  const trainingSku = {
    sku_id: 1, name: 'Synthetic SKU', initial_stock: 1,
    initial_pipeline: [0, 0, 0, 0, 0, 0], history: [1, 1],
    demand: [1, 1, 1, 1, 1, 2], lead_time: 1, case_pack: 1, min_order: 0,
    holding_cost: 0, shortage_cost: 10, fixed_order_cost: 5,
  };
  const sourcePayload = {
    capacity: 10, skus: [trainingSku], training_start: '2026-08-01',
    training_end: '2026-08-06', history_start: '2026-07-30',
    history_end: '2026-07-31', inventory_as_of: '2026-08-01',
    demand_observation: 'true_demand',
  };
  const source = await stack.evolveCall('evolve_prepare_training_source', {
    source_id: `${id}-source`, source_json: JSON.stringify(sourcePayload),
  });
  expect(source.training_source_id).toBe(`${id}-source`);
  expect(source.normalized_sha256).toMatch(/^[a-f0-9]{64}$/);
  expect(source.raw_sha256).toMatch(/^[a-f0-9]{64}$/);

  const holdoutSku = { ...trainingSku,
    name: 'PRIVATE_HOLDOUT_OUTCOMES_SENTINEL', demand: [1, 1, 1, 1, 1, 1] };
  const serviceTargets = { aggregate_min_fill_rate: 0.8,
    per_sku_min_fill_rate: { '1': 0.8 } };
  const holdout = await stack.evolveCall('evolve_register_holdout', {
    holdout_id: `${id}-holdout`, source_sha256: 'c'.repeat(64),
    training_source_id: source.training_source_id,
    training_source_sha256: source.normalized_sha256,
    source_ref: `synthetic:${id}:holdout`, training_source_ref: `synthetic:${id}:training`,
    training_start: '2026-08-01', training_end: '2026-08-06',
    history_start: '2026-08-30', history_end: '2026-08-31',
    inventory_as_of: '2026-09-01', holdout_start: '2026-09-01',
    holdout_end: '2026-09-06', outcomes_sealed_before_search: true,
    outcomes_publicly_disclosed: true, demand_observation: 'true_demand',
    problem: { capacity: 10, skus: [holdoutSku] }, service_targets: serviceTargets,
    baseline_sql: baseline, max_cost_ratio: 0.9,
    evaluator_version: 'replenishment_decision_v2',
    planning_window_days: 2, scored_window_days: 2,
    sensitivity_tail_days: [1, 4], unit_order_costs: { '1': 1 },
    terminal_stock_tolerance: { '1': 0 },
  });
  expect(holdout.holdout_sha256).toMatch(/^[a-f0-9]{64}$/);

  const search = {
    pilot: 'p1_decision', brain: 'llm', evaluator_version: 'replenishment_decision_v2',
    seed_sql: seed, baseline_sql: baseline, capacity: 10, skus: sourcePayload.skus,
    service_targets: serviceTargets, planning_window_days: 2, scored_window_days: 2,
    unit_order_costs: { '1': 1 }, terminal_stock_tolerance: { '1': 0 },
    holdout_id: `${id}-holdout`, training_source_id: source.training_source_id,
    source_sha256: source.normalized_sha256,
    training_start: '2026-08-01', training_end: '2026-08-06',
    history_start: '2026-07-30', history_end: '2026-07-31',
    inventory_as_of: '2026-08-01', demand_observation: 'true_demand',
    max_generations: 0, budget: { max_evaluated: 1 },
  };
  const pageId = `markdown/instances/evolve_problem/${id}.md`;
  const content = `---\ntype: instance\nskill: evolve_problem\nid: ${id}\nowner_subject: alice\npilot: p1_decision\nsearch_request: ${JSON.stringify(search)}\n---\n# Synthetic seed admission\n`;
  expect((await stack.call('update_page', { page_id: pageId,
    content, base_sha256: '' })).ok).toBe(true);
  const revision = (await stack.call('expand', { page_id: pageId, raw: true })).content_sha256;
  expect(revision).toMatch(/^[a-f0-9]{64}$/);
  stack.setGeminiPlanTarget(pageId, revision as string);

  const knowledge = pane(stack.page, 'Knowledge');
  await knowledge.getByRole('treeitem', { name: /^evolve_problem/ }).click();
  await knowledge.getByRole('treeitem', { name: new RegExp(id) }).click();
  const pageUi = await webviewWith(stack.page, 'escurel-page-as-ui');
  await pageUi.getByRole('button', { name: 'Review experiment plan', exact: true }).click();

  let preflight: EventRow | undefined;
  await expect.poll(async () => {
    const result = await stack.call('list_events', { label_skill: 'evolve_preflight', limit: 100 });
    preflight = (result.events as EventRow[]).find((event) => event.instance_page_id === pageId);
    return preflight?.event_id;
  }, { timeout: 30_000 }).toBeTruthy();
  let ready: EventRow | undefined;
  await expect.poll(async () => {
    const result = await stack.call('list_events', { root_event_id: preflight!.event_id,
      label_skill: 'evolve:preflight', include_system: true });
    ready = (result.events as EventRow[]).find((event) => event.title === 'problem-structure-checked');
    return ready?.event_id;
  }, { timeout: 30_000 }).toBeTruthy();
  const report = JSON.parse(ready!.body) as Record<string, unknown>;
  expect(report.problem_sha256).toBe(revision);
  expect(report.structural_ready_for_start).toBe(true);
  expect(ready!.body).not.toContain('initial_pipeline');
  expect(ready!.body).not.toContain(holdoutSku.name);
  const contractDialog = stack.page.getByRole('dialog', { name: 'Info' })
    .filter({ hasText: 'Review the frozen private holdout contract' });
  await expect(contractDialog).toContainText(`synthetic:${id}:holdout`);
  await expect(contractDialog).toContainText(String(holdout.holdout_sha256));
  await contractDialog.getByRole('button', { name: 'Review experiment plan' }).click();

  let planEvent: EventRow | undefined;
  await expect.poll(async () => {
    const result = await stack.call('list_events', { label_skill: 'evolve_run', limit: 100 });
    planEvent = (result.events as EventRow[]).find((event) => event.instance_page_id === pageId
      && event.provenance?.manual?.mode === 'plan');
    return planEvent?.event_id;
  }, { timeout: 30_000 }).toBeTruthy();
  let runId: string | undefined;
  await expect.poll(async () => {
    const lineage = await stack.call('list_lineage', { root_event_id: planEvent!.event_id });
    runId = (lineage.nodes as Array<{ type: string; state: string; id: string }> | undefined)
      ?.find((node) => node.type === 'run' && node.state === 'planned')?.id;
    return runId;
  }, { timeout: 45_000 }).toBeTruthy();
  expect(stack.geminiRequests).toHaveLength(3);
  for (const request of stack.geminiRequests) {
    expect(JSON.stringify(request)).not.toContain(holdoutSku.name);
    expect(JSON.stringify(request)).not.toContain('synthetic:' + id + ':holdout');
  }

  const thread = await webviewWith(stack.page, 'escurel-thread-canvas');
  await thread.locator(`escurel-thread-canvas .card.type-run[data-node-id="${runId}"]`).click();
  await thread.getByRole('button', { name: 'Approve plan' }).click();
  const approvalDialog = stack.page.getByRole('dialog', { name: 'Warning' })
    .filter({ hasText: 'Approve this Evolve search' });
  await expect(approvalDialog).toContainText('Plan harness: gemini');
  await approvalDialog.getByRole('button', { name: 'Approve search' }).click();

  let approvalId: string | undefined;
  await expect.poll(async () => {
    const result = await stack.call('list_events', { label_skill: 'evolve_run', limit: 100 });
    approvalId = (result.events as EventRow[]).find((event) =>
      event.event_id === `evolve-approval-${runId}`)?.event_id;
    return approvalId;
  }, { timeout: 30_000 }).toBeTruthy();
  let admission: EventRow | undefined;
  await expect.poll(async () => {
    const result = await stack.call('list_events', { root_event_id: approvalId,
      label_skill: 'evolve:admission', include_system: true });
    admission = (result.events as EventRow[]).find((event) => event.title === 'experiment-admitted');
    return admission?.event_id;
  }, { timeout: 60_000 }).toBeTruthy();
  const experimentId = admission!.provenance?.evolve?.experiment_id;
  expect(experimentId).toEqual(expect.any(String));
  expect(admission!.body).toContain(`[[evolve_experiment::${experimentId}]]`);
  expect(admission!.kind).toBe('system');
  expect(admission!.source).toBe('anofox-evolve');
  expect(admission!.root_event_id).toBe(approvalId);
  expect(admission!.provenance?.evolve?.approval_event_id).toBe(approvalId);
  expect(admission!.provenance?.evolve?.problem_sha256).toBe(revision);

  let completedStatus: Record<string, unknown> | undefined;
  await expect.poll(async () => {
    completedStatus = await stack.evolveCall('evolve_status', { experiment: experimentId });
    return completedStatus.dispatch_state;
  }, { timeout: 60_000 }).toBe('completed');
  expect(completedStatus!.bound_holdout_sha256).toBe(holdout.holdout_sha256);
  expect(completedStatus!.bound_training_source_id).toBe(source.training_source_id);
  expect(completedStatus!.training_source_binding).toBe('server_hashed_submitted_json');
  expect(completedStatus!.validation_effective_passed).toBe(false);
  expect(completedStatus!.promotable).toBe(false);
  expect(completedStatus!.next_validation_action).toBe('evolve_validate_winner');
  expect((completedStatus!.budget_spent as Record<string, unknown>).candidates_evaluated).toBe(1);
  expect(completedStatus!.best_program_id).toEqual(expect.any(Number));
  const best = await stack.evolveCall('evolve_best', { experiment: experimentId });
  expect(best.id).toBe(completedStatus!.best_program_id);
  expect(best.origin).toBe('seed');
  expect(best.generation).toBe(0);
  const experimentPageId = `markdown/instances/evolve_experiment/${experimentId}.md`;
  await expect.poll(async () => {
    try {
      const page = await stack.call('expand', { page_id: experimentPageId });
      return (page.frontmatter as Record<string, unknown>)?.status;
    } catch { return undefined; }
  }, { timeout: 60_000 }).toBe('completed');
  const approvalThread = await webviewWith(stack.page, 'escurel-thread-canvas');
  await approvalThread.locator(`escurel-thread-canvas .card[data-node-id="${admission!.event_id}"]`).click();
  await approvalThread.locator('escurel-thread-inspector .wikilink').click();
  await expect(stack.page.getByRole('tab', { name: new RegExp(String(experimentId)), selected: true })).toBeVisible();
  const experimentUi = await webviewWith(stack.page, 'escurel-page-as-ui');
  await expect(experimentUi.getByText('completed', { exact: true })).toBeVisible();
  await experimentUi.getByRole('button', { name: 'Validate winner', exact: true }).click();

  let validationEvent: EventRow | undefined;
  await expect.poll(async () => {
    const result = await stack.call('list_events', { label_skill: 'evolve_validate', limit: 100 });
    validationEvent = (result.events as EventRow[]).find((event) =>
      event.instance_page_id === experimentPageId);
    return validationEvent?.event_id;
  }, { timeout: 30_000 }).toBeTruthy();
  let validation: Record<string, unknown> | undefined;
  await expect.poll(async () => {
    validation = await stack.evolveCall('evolve_validation', { experiment: experimentId });
    return validation.state;
  }, { timeout: 60_000 }).toBe('passed');
  expect(validation!.effective_passed).toBe(true);
  const validationReport = validation!.report as Record<string, unknown>;
  expect(validationReport.evaluator).toBe('replenishment_decision_v2');
  expect(validationReport.winner_program_id).toBe(best.id);
  expect(validationReport.training_source_id).toBe(source.training_source_id);
  expect(validationReport.training_source_sha256).toBe(source.normalized_sha256);
  expect(validationReport.submitted_data_scope).toBe('operator_labeled_synthetic_fixture_engineering_only');
  expect(validationReport.outcomes_publicly_disclosed).toBe(true);
  expect(validationReport.comparisons).toHaveLength(2);

  let validationReceipt: EventRow | undefined;
  await expect.poll(async () => {
    const result = await stack.call('list_events', { root_event_id: validationEvent!.event_id,
      label_skill: 'evolve:validation', include_system: true });
    validationReceipt = (result.events as EventRow[]).find((event) =>
      event.title === 'winner-validation-final');
    return validationReceipt?.event_id;
  }, { timeout: 60_000 }).toBeTruthy();
  expect(validationReceipt!.body).toContain('passed its predeclared checks');
  expect(validationReceipt!.body).toContain('Published synthetic fixture: sandbox demo only');
  const reportPageId = `markdown/instances/evolve_validation_report/${experimentId}.md`;
  await expect.poll(async () => {
    try {
      const page = await stack.call('expand', { page_id: reportPageId });
      return (page.frontmatter as Record<string, unknown>)?.effective_passed;
    } catch { return undefined; }
  }, { timeout: 60_000 }).toBe(true);
  const validationThread = await webviewWith(stack.page, 'escurel-thread-canvas');
  await validationThread.locator(`escurel-thread-canvas .card[data-node-id="${validationReceipt!.event_id}"]`).click();
  await validationThread.locator('escurel-thread-inspector .wikilink').nth(1).click();
  await expect(stack.page.getByRole('tab', { name: new RegExp(String(experimentId)), selected: true })).toBeVisible();
  const reportUi = await webviewWith(stack.page, 'escurel-page-as-ui');
  await expect(reportUi.getByText('Published synthetic fixture', { exact: false })).toBeVisible();
});
