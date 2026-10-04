import type { Page } from '@playwright/test';
import { createHash } from 'node:crypto';
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

test('native owner approval, two proposal generations, validation, and inactive candidate through Evolve', async ({ stack }) => {
  const id = `native-service-${Date.now()}`;
  const baselineRule = 'CASE WHEN period % 6 = 0 THEN 2 WHEN period % 6 IN (1, 2, 3, 4) THEN 1 ELSE 0 END';
  const winnerRule = 'CASE WHEN period % 3 = 0 THEN 3 ELSE 0 END';
  const policySql = (rule: string) => `SELECT sku_id, (${rule} * CASE WHEN sku_id = 1 THEN 1 ELSE 2 END)::BIGINT AS order_qty FROM p1_observation`;
  const baseline = policySql(baselineRule);
  const winnerSql = policySql(winnerRule);
  const pipeline = Array(12).fill(0) as number[];
  const trainingSku = {
    sku_id: 1, name: 'Synthetic SKU A', initial_stock: 1,
    initial_pipeline: pipeline, history: [1],
    demand: Array(12).fill(1) as number[], lead_time: 1, case_pack: 1, min_order: 0,
    holding_cost: 0, shortage_cost: 100, fixed_order_cost: 5,
  };
  const trainingSkuB = { ...trainingSku,
    sku_id: 2, name: 'Synthetic SKU B', initial_stock: 2,
    history: [2], demand: Array(12).fill(2) as number[] };
  const sourcePayload = {
    capacity: 9, skus: [trainingSku, trainingSkuB], training_start: '2026-08-01',
    training_end: '2026-08-12', history_start: '2026-07-31',
    history_end: '2026-07-31', inventory_as_of: '2026-08-01',
    demand_observation: 'true_demand',
  };
  const source = await stack.evolveCall('evolve_prepare_training_source', {
    source_id: `${id}-source`, source_json: JSON.stringify(sourcePayload),
  });
  expect(source.training_source_id).toBe(`${id}-source`);
  expect(source.normalized_sha256).toMatch(/^[a-f0-9]{64}$/);
  expect(source.raw_sha256).toMatch(/^[a-f0-9]{64}$/);

  const holdoutSku = { ...trainingSku, name: 'PRIVATE_HOLDOUT_OUTCOMES_SENTINEL_A' };
  const holdoutSkuB = { ...trainingSkuB, name: 'PRIVATE_HOLDOUT_OUTCOMES_SENTINEL_B' };
  const serviceTargets = { aggregate_min_fill_rate: 0.8,
    per_sku_min_fill_rate: { '1': 0.8, '2': 0.8 } };
  const holdout = await stack.evolveCall('evolve_register_holdout', {
    holdout_id: `${id}-holdout`, source_sha256: 'c'.repeat(64),
    training_source_id: source.training_source_id,
    training_source_sha256: source.normalized_sha256,
    source_ref: `synthetic:${id}:holdout`, training_source_ref: `synthetic:${id}:training`,
    training_start: '2026-08-01', training_end: '2026-08-12',
    history_start: '2026-08-31', history_end: '2026-08-31',
    inventory_as_of: '2026-09-01', holdout_start: '2026-09-01',
    holdout_end: '2026-09-12', outcomes_sealed_before_search: true,
    outcomes_publicly_disclosed: true, demand_observation: 'true_demand',
    problem: { capacity: 9, skus: [holdoutSku, holdoutSkuB] }, service_targets: serviceTargets,
    baseline_sql: baseline, max_cost_ratio: 0.9,
    evaluator_version: 'replenishment_decision_v2',
    planning_window_days: 1, scored_window_days: 3,
    sensitivity_tail_days: [3, 9], unit_order_costs: { '1': 1, '2': 1 },
    terminal_stock_tolerance: { '1': 0, '2': 0 },
  });
  expect(holdout.holdout_sha256).toMatch(/^[a-f0-9]{64}$/);

  const search = {
    pilot: 'p1_decision', brain: 'llm', evaluator_version: 'replenishment_decision_v2',
    seed_sql: baseline, baseline_sql: baseline, capacity: 9, skus: sourcePayload.skus,
    service_targets: serviceTargets, planning_window_days: 1, scored_window_days: 3,
    unit_order_costs: { '1': 1, '2': 1 }, terminal_stock_tolerance: { '1': 0, '2': 0 },
    holdout_id: `${id}-holdout`, training_source_id: source.training_source_id,
    source_sha256: source.normalized_sha256,
    training_start: '2026-08-01', training_end: '2026-08-12',
    history_start: '2026-07-31', history_end: '2026-07-31',
    inventory_as_of: '2026-08-01', demand_observation: 'true_demand',
    max_generations: 2, budget: { max_evaluated: 3 },
    synthetic_brain: 'batch_progression_v1',
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
    ready = (result.events as EventRow[])[0];
    return ready?.event_id;
  }, { timeout: 30_000 }).toBeTruthy();
  expect(ready!.title, ready!.body).toBe('problem-structure-checked');
  const report = JSON.parse(ready!.body) as Record<string, unknown>;
  expect(report.problem_sha256).toBe(revision);
  expect(report.structural_ready_for_start).toBe(true);
  expect(ready!.body).not.toContain('initial_pipeline');
  expect(ready!.body).not.toContain(holdoutSku.name);
  expect(ready!.body).not.toContain(holdoutSkuB.name);
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
    expect(JSON.stringify(request)).not.toContain(holdoutSkuB.name);
    expect(JSON.stringify(request)).not.toContain('synthetic:' + id + ':holdout');
  }

  const thread = await webviewWith(stack.page, 'escurel-thread-canvas');
  await thread.locator(`escurel-thread-canvas .card.type-run[data-node-id="${runId}"]`).click();
  await thread.getByRole('button', { name: 'Approve plan' }).click();
  const approvalDialog = stack.page.getByRole('dialog', { name: 'Warning' })
    .filter({ hasText: 'Approve this Evolve search' });
  await expect(approvalDialog).toContainText('Plan harness: gemini');
  await expect(approvalDialog).toContainText('Proposal source: deterministic synthetic fixture');
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
  expect(completedStatus!.proposal_source).toBe('synthetic_batch_progression_v1_not_model_judgment');
  expect(completedStatus!.training_source_binding).toBe('server_hashed_submitted_json');
  expect(completedStatus!.validation_effective_passed).toBe(false);
  expect(completedStatus!.promotable).toBe(false);
  expect(completedStatus!.next_validation_action).toBe('evolve_validate_winner');
  expect((completedStatus!.budget_spent as Record<string, unknown>).candidates_evaluated).toBe(3);
  expect((completedStatus!.budget_spent as Record<string, unknown>).candidates_generated).toBe(3);
  expect(completedStatus!.generation).toBe(2);
  expect(completedStatus!.best_program_id).toEqual(expect.any(Number));
  const best = await stack.evolveCall('evolve_best', { experiment: experimentId });
  expect(best.id).toBe(completedStatus!.best_program_id);
  expect(best.origin).toBe('synthetic_fixture');
  expect(best.generation).toBe(2);
  expect(best.parent_id).toEqual(expect.any(Number));
  const lifecycle = await stack.evolveCall('evolve_events', { experiment: experimentId });
  const improvements = (lifecycle.events as Array<{ kind: string; payload: Record<string, unknown> }>)
    .filter((event) => event.kind === 'new_best');
  expect(improvements).toHaveLength(2);
  const proposalIds = improvements.map((event) => event.payload.program_id);
  expect(new Set(proposalIds).size).toBe(2);
  expect(proposalIds[1]).toBe(best.id);
  expect(best.parent_id).toBe(proposalIds[0]);
  expect(improvements[1]!.payload.combined_score).toBeGreaterThan(improvements[0]!.payload.combined_score as number);
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
  await expect(experimentUi.getByText('Proposal source: deterministic synthetic fixture', { exact: false })).toBeVisible();
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
  expect(validationReport.winner_sql_sha256).toBe(createHash('sha256').update(winnerSql).digest('hex'));
  expect(validationReport.baseline_sql_sha256).toBe(createHash('sha256').update(baseline).digest('hex'));
  expect(validationReport.holdout_sha256).toBe(holdout.holdout_sha256);
  expect(validationReport.source_binding_kind).toBe('server_hashed_submitted_json');
  expect(validationReport.training_source_id).toBe(source.training_source_id);
  expect(validationReport.training_source_sha256).toBe(source.normalized_sha256);
  expect(validationReport.submitted_data_scope).toBe('operator_labeled_synthetic_fixture_engineering_only');
  expect(validationReport.outcomes_publicly_disclosed).toBe(true);
  expect(validationReport.comparisons).toHaveLength(2);
  expect(validationReport.paired_replay_checks_pass).toBe(true);
  expect(validationReport.cost_improved).toBe(true);
  for (const comparison of validationReport.comparisons as Array<Record<string, unknown>>) {
    expect(comparison.pass).toBe(true);
    expect(comparison.cost_improved).toBe(true);
    expect(comparison.violations).toEqual([]);
    expect((comparison.terminal_comparison as Record<string, unknown>).violations).toEqual([]);
    for (const policy of ['baseline', 'candidate']) {
      const result = comparison[policy] as Record<string, unknown>;
      expect((result.full_service as Record<string, unknown>).feasible).toBe(true);
      expect((result.scored_service as Record<string, unknown>).feasible).toBe(true);
    }
  }

  let validationReceipt: EventRow | undefined;
  await expect.poll(async () => {
    const result = await stack.call('list_events', { root_event_id: validationEvent!.event_id,
      label_skill: 'evolve:validation', include_system: true });
    validationReceipt = (result.events as EventRow[]).find((event) =>
      event.title === 'winner-validation-final');
    return validationReceipt?.event_id;
  }, { timeout: 60_000 }).toBeTruthy();
  expect(validationReceipt!.body).toContain('passed its predeclared checks');
  expect(validationReceipt!.body).toContain('Publicly disclosed synthetic fixture: sandbox demo only');
  const reportPageId = `markdown/instances/evolve_validation_report/${experimentId}.md`;
  let reportPage: Record<string, unknown> | undefined;
  await expect.poll(async () => {
    try {
      const page = await stack.call('expand', { page_id: reportPageId });
      reportPage = page.frontmatter as Record<string, unknown>;
      return reportPage?.effective_passed;
    } catch { return undefined; }
  }, { timeout: 60_000 }).toBe(true);
  expect(reportPage!.candidate_use).toBe('sandbox_demo_only');
  expect(reportPage!.winner_program_id).toBe(best.id);
  expect(reportPage!.winner_sql_sha256).toBe(validationReport.winner_sql_sha256);
  expect(reportPage!.report_sha256).toBe(validation!.report_sha256);
  expect(reportPage!.next_candidate_action).toBe('evolve_publish_candidate');
  const finalStatus = await stack.evolveCall('evolve_status', { experiment: experimentId });
  expect(finalStatus.operational_activation_available).toBe(false);
  const validationThread = await webviewWith(stack.page, 'escurel-thread-canvas');
  await validationThread.locator(`escurel-thread-canvas .card[data-node-id="${validationReceipt!.event_id}"]`).click();
  await validationThread.locator('escurel-thread-inspector .wikilink').nth(1).click();
  await expect(stack.page.getByRole('tab', { name: new RegExp(String(experimentId)), selected: true })).toBeVisible();
  const reportUi = await webviewWith(stack.page, 'escurel-page-as-ui');
  await expect(reportUi.getByText(/State:\s*passed/)).toBeVisible();
  await expect(reportUi.getByText('Publicly disclosed synthetic fixture', { exact: false })).toBeVisible();
  await expect(reportUi.getByRole('button', { name: 'Create policy candidate', exact: true })).toBeVisible();
  await reportUi.getByRole('button', { name: 'Create policy candidate', exact: true }).click();
  const candidateDialog = stack.page.getByRole('dialog', { name: 'Warning' })
    .filter({ hasText: 'Create an inactive policy candidate' });
  await expect(candidateDialog).toContainText(`winner ${best.id}`);
  await expect(candidateDialog).toContainText('This does not activate a policy');
  await expect(candidateDialog).toContainText('sandbox use only');
  await expect(candidateDialog).toContainText('must never be activated as an operational policy');
  await candidateDialog.getByRole('button', { name: 'Create candidate' }).click();
  const reviewNote = stack.page.locator('.quick-input-widget input').first();
  await expect(reviewNote).toBeVisible();
  await reviewNote.fill('Reviewed disclosed synthetic evidence for integration only');
  await stack.page.keyboard.press('Enter');

  let candidateEvent: EventRow | undefined;
  await expect.poll(async () => {
    const result = await stack.call('list_events', { label_skill: 'evolve_publish_candidate', limit: 100 });
    candidateEvent = (result.events as EventRow[]).find((event) =>
      event.instance_page_id === reportPageId);
    return candidateEvent?.event_id;
  }, { timeout: 30_000 }).toBeTruthy();
  let candidateReceipt: EventRow | undefined;
  await expect.poll(async () => {
    const result = await stack.call('list_events', { root_event_id: candidateEvent!.event_id,
      label_skill: 'evolve:candidate', include_system: true });
    candidateReceipt = (result.events as EventRow[]).find((event) =>
      event.title === 'candidate-publication-final');
    return candidateReceipt?.event_id;
  }, { timeout: 60_000 }).toBeTruthy();
  expect(candidateReceipt!.body).toContain('inactive candidate');
  expect(candidateReceipt!.body).toContain('sandbox demo only');
  const policyId = candidateReceipt!.body.match(/\[\[plan_policy::([^\]]+)\]\]/)?.[1];
  expect(policyId).toBe(`${experimentId}-${best.id}`);
  const policyPageId = `markdown/instances/plan_policy/${policyId}.md`;
  const policyPage = await stack.call('expand', { page_id: policyPageId });
  const policy = policyPage.frontmatter as Record<string, unknown>;
  expect(policy.status).toBe('candidate');
  expect(policy.candidate_use).toBe('sandbox_demo_only');
  expect(policy.activation_status).toBe('not_activated');
  expect((policy.acl as Record<string, unknown>).read).toEqual(['owner']);
  expect(policy.program_id).toBe(best.id);
  expect(policy.validation_report_sha256).toBe(reportPage!.report_sha256);
  expect(policy.validation_ref).toBe(`decision-validation::${experimentId}`);
  expect(policy.candidate_kind).toBe('synthetic_sandbox_candidate');
  expect(policy.validation_evidence_scope).toBe('disclosed_synthetic_replay_two_nested_tails');
  expect(policyPage.body).toContain(winnerSql);
  const candidateThread = await webviewWith(stack.page, 'escurel-thread-canvas');
  await candidateThread.locator(`escurel-thread-canvas .card[data-node-id="${candidateReceipt!.event_id}"]`).click();
  await candidateThread.locator('escurel-thread-inspector .wikilink').click();
  await expect(stack.page.getByRole('tab', { name: new RegExp(String(policyId)), selected: true })).toBeVisible();
  const policyUi = await webviewWith(stack.page, 'escurel-page-as-ui');
  await expect(policyUi.getByText('Publicly disclosed synthetic fixture', { exact: false })).toBeVisible();
  await expect(policyUi.getByText('not_activated', { exact: true })).toBeVisible();
  await expect(policyUi.getByText('Reviewed disclosed synthetic evidence for integration only')).toBeVisible();
});
