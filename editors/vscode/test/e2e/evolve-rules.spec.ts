import type { Page } from '@playwright/test';
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from './fixtures';
import { filterCommandPalette } from './helpers';

/**
 * Discovered data rules, reviewed and approved by a person inside VS Code, against a real running
 * evolve-agent doing real rule discovery on replenishment data in the demo's own format.
 *
 * propose -> one confirmation test -> a value-free report (counts, p-values, Holm decision, required
 * rows, insufficient evidence) -> the person chooses rules and confirms -> approve -> the approved
 * hash is attached to a NEW experiment start. Nothing is approved without the explicit click.
 *
 * Needs EVOLVE_AGENT_BIN (an evolve-agent build with the rule tools) and ANOFOX_EXTENSION_DIR pointing at
 * a DuckDB 1.5.6 extension profile, like the scenarios spec. The agent is started with alice as the
 * only rule approver.
 */
test.use({ evolveAgentBin: process.env.EVOLVE_AGENT_BIN });
test.skip(!process.env.EVOLVE_AGENT_BIN, 'Set EVOLVE_AGENT_BIN to test the rule review');

const SLOW = Number(process.env.ESCUREL_E2E_SLOW) || 1;

/** A deterministic generator (mulberry32): the same data on every run. */
function rng(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** The demo's replenishment source payload at the contract's size: 16 SKUs, 90 training days, demand a multiple of 6. */
function richSource(seed: number, days = 90) {
  const next = rng(seed);
  const series = (n: number) => Array.from({ length: n }, () => 6 * Math.floor(next() * 7));
  return {
    capacity: 100000,
    skus: Array.from({ length: 16 }, (_, i) => ({
      sku_id: i + 1,
      name: `S${i + 1}`,
      initial_stock: 10,
      initial_pipeline: Array(days).fill(0),
      history: series(365),
      demand: series(days),
      lead_time: 2,
      case_pack: 6,
      min_order: 0,
      holding_cost: 0.1,
      shortage_cost: 5.0,
      fixed_order_cost: 2.0,
    })),
    training_start: '2026-08-01',
    training_end: days === 90 ? '2026-10-29' : '2026-09-19',
    history_start: '2025-08-01',
    history_end: '2026-07-31',
    inventory_as_of: '2026-08-01',
    demand_observation: 'true_demand',
  };
}

interface BoundEvidence {
  rule_sha256: string;
  decision: string;
  confirmation: { rows: number; breaches: number };
  breach_rate_upper_bound?: number;
  bound_statement?: string;
}

const dialog = (page: Page, text: string | RegExp) =>
  page.locator('[role="dialog"]', { hasText: text });
const notice = (page: Page, text: string | RegExp) =>
  page.locator('.notifications-toasts', { hasText: text }).first();

/**
 * The test-policy question that precedes the one-shot test: Enter keeps the service default, or the
 * person types p_max (and alpha) as percentages. A value above the cap is refused while typing.
 */
async function answerPolicy(page: Page, choice?: { pMax: string; alpha: string }) {
  const picker = page.locator('.quick-input-widget');
  await expect(picker.getByPlaceholder(/Test policy\. The test runs once per batch/)).toBeVisible({
    timeout: 60_000 * SLOW,
  });
  if (!choice) {
    await page.keyboard.press('Enter');
    return;
  }
  await page.keyboard.press('ArrowDown');
  await page.keyboard.press('Enter');
  const input = picker.locator('input');
  await expect(picker.locator('.quick-input-title')).toContainText('p_max');
  // Above the service cap: refused client-side with the cap in the message, Enter does not accept.
  await input.fill('6');
  await expect(picker.locator('.quick-input-message')).toContainText('at most 5%');
  await input.press('Enter');
  await expect(picker.locator('.quick-input-title')).toContainText('p_max');
  await input.fill(choice.pMax);
  await input.press('Enter');
  await expect(picker.locator('.quick-input-title')).toContainText('alpha');
  await input.fill('11');
  await expect(picker.locator('.quick-input-message')).toContainText('at most 10%');
  await input.fill(choice.alpha);
  await input.press('Enter');
}

async function runReview(page: Page, sourceId: string, choice?: { pMax: string; alpha: string }) {
  // A toast from the previous step can still hold focus, and the palette then opens without its input.
  await expect(page.locator('.notifications-toasts .notification-toast')).toHaveCount(0, {
    timeout: 60_000 * SLOW,
  });
  await filterCommandPalette(
    page,
    'Discover, review and approve Anofox Evolve data rules',
    'Discover, review and approve Anofox Evolve data rules',
  );
  const input = page.locator('.quick-input-widget input');
  await input.press('Enter');
  await expect(page.locator('.quick-input-widget')).toContainText(
    'training-source ID to discover rules in',
  );
  await input.fill(sourceId);
  await input.press('Enter');
  await answerPolicy(page, choice);
  const testDialog = dialog(page, 'Run the one confirmation test');
  await expect(testDialog).toBeVisible({ timeout: 60_000 * SLOW });
  return testDialog;
}

/**
 * The review document is a virtual editor and Monaco draws only the visible lines, so a line deep in
 * the report is found the way a person would: with Find. True when the document contains `text`.
 */
async function documentContains(page: Page, text: string): Promise<boolean> {
  await page.locator('.monaco-editor .view-lines').last().click();
  await page.keyboard.press('Control+F');
  const find = page.locator('.monaco-editor .find-widget textarea').first();
  await find.fill(text);
  await find.press('Enter');
  const count = page.locator('.monaco-editor .find-widget .matchesCount').first();
  await expect(count).not.toHaveText('', { timeout: 20_000 * SLOW });
  const found = !/No results/i.test((await count.innerText()).trim());
  if (found)
    await expect(page.locator('.monaco-editor .view-lines').last()).toContainText(
      text.slice(0, 40),
    );
  await page.keyboard.press('Escape');
  return found;
}

test('a person reviews and approves discovered rules, and the approval rides on a new experiment start', async ({
  stack,
}) => {
  test.setTimeout(420_000 * SLOW);
  const id = `rules-${Date.now()}`;

  // The demo's own replenishment training source (12 days, 2 SKUs) and one at the contract's size.
  const demo = JSON.parse(
    readFileSync(join(__dirname, '../../demo/evolve/p1-replenishment.json'), 'utf8'),
  ) as { setup: { training_source: { payload: unknown } } };
  const smallId = `${id}-demo`;
  const richId = `${id}-rich`;
  await stack.evolveCall('evolve_prepare_training_source', {
    source_id: smallId,
    source_json: JSON.stringify(demo.setup.training_source.payload),
  });
  const richPayload = richSource(7);
  const rich = await stack.evolveCall('evolve_prepare_training_source', {
    source_id: richId,
    source_json: JSON.stringify(richPayload),
  });

  // 1. Too little data: the report says insufficient evidence and offers nothing to approve.
  let confirmTest = await runReview(stack.page, smallId);
  await confirmTest.getByRole('button', { name: 'Run confirmation test' }).click();
  await expect(stack.page.getByRole('tab', { name: /rules-[a-f0-9]{12}\.txt/ })).toBeVisible({
    timeout: 60_000 * SLOW,
  });
  await expect(stack.page.locator('.monaco-editor .view-lines').last()).toContainText(
    'Discovered rules: confirmation test',
  );
  await expect(notice(stack.page, 'nothing can be approved')).toBeVisible();
  await stack.shot('evolve-rules-insufficient-evidence');
  await stack.page.keyboard.press('Control+W');

  // 1b. A rule can have a LOW upper bound and still be insufficient evidence. 16 SKUs x 50 days at
  // the service default p_max (1%): 400 confirmation rows, 0 breaches, bound below 1%, but the test
  // needs ~670 rows. The document shows the bound and says it does not make the rule supported.
  const shortId = `${id}-short`;
  await stack.evolveCall('evolve_prepare_training_source', {
    source_id: shortId,
    source_json: JSON.stringify(richSource(7, 50)),
  });
  confirmTest = await runReview(stack.page, shortId);
  const defaultDetail = await confirmTest.innerText();
  expect(defaultDetail).toContain('p_max the service default, alpha the service default');
  expect(defaultDetail).toMatch(/one-shot per batch/);
  await confirmTest.getByRole('button', { name: 'Run confirmation test' }).click();
  await expect(
    stack.page
      .locator('.quick-input-widget')
      .getByPlaceholder(/Choose the supported rules to approve/),
  ).toBeVisible({ timeout: 90_000 * SLOW });
  await stack.page.keyboard.press('Escape'); // approve nothing
  const shortBatch = (await stack.evolveCall('evolve_rules_propose', {
    training_source_id: shortId,
  })) as { batch_id: string };
  const shortTest = (
    (await stack.evolveCall('evolve_rules_get', { batch_id: shortBatch.batch_id })) as {
      test: { policy: { p_max: number }; evidence: BoundEvidence[] };
    }
  ).test;
  const lowInsufficient = shortTest.evidence.find(
    (e) =>
      e.decision === 'insufficient_evidence' &&
      e.breach_rate_upper_bound! < shortTest.policy.p_max &&
      e.confirmation.breaches === 0,
  );
  expect(lowInsufficient, 'a real insufficient rule with a bound below p_max').toBeDefined();
  expect(await documentContains(stack.page, lowInsufficient!.bound_statement!)).toBe(true);
  expect(await documentContains(stack.page, 'does not make it supported')).toBe(true);
  expect(await documentContains(stack.page, 'not an approval signal')).toBe(true);
  await stack.shot('evolve-rules-low-bound-insufficient');
  await stack.page.keyboard.press('Control+W');

  // 2. Enough data: review, choose, and CANCEL at the final confirmation. Nothing is approved.
  confirmTest = await runReview(stack.page, richId, { pMax: '5', alpha: '5' });
  const candidates = await confirmTest.innerText();
  expect(candidates).toContain('is a multiple of 6');
  // The modal states the chosen policy, the caps and that the test cannot be redone, before running.
  expect(candidates).toContain('p_max 5% (0.05), alpha 5% (0.05)');
  expect(candidates).toContain('p_max up to 5%, alpha up to 10%');
  expect(candidates).toMatch(/one-shot per batch/);
  expect(candidates).toMatch(/cannot be redone/);
  await stack.shot('evolve-rules-confirm-test');
  await confirmTest.getByRole('button', { name: 'Run confirmation test' }).click();
  await expect(stack.page.locator('.monaco-editor .view-lines').last()).toContainText(
    'Discovered rules: confirmation test',
    { timeout: 90_000 * SLOW },
  );
  const reportText = (
    await stack.page.locator('.monaco-editor .view-lines').last().innerText()
  ).replace(/\u00a0/g, ' ');
  expect(reportText).toMatch(/Result: [1-9]\d* supported/);
  expect(reportText).toContain('Holm-corrected');
  expect(reportText).toContain('binom_holm_ub_v2 (p_max 5%, alpha 5%)');
  expect(reportText).toContain('Caps for new tests: p_max may be up to 5%, alpha up to 10%');
  await stack.shot('evolve-rules-report');
  const chooser = stack.page.locator('.quick-input-widget');
  await expect(chooser.getByPlaceholder(/Choose the supported rules to approve/)).toBeVisible();
  // Nothing is pre-selected, so Enter on an empty selection approves nothing.
  const rows = chooser.locator('.quick-input-list .monaco-list-row');
  await expect(rows.first()).toBeVisible();
  const offered = await rows.count();
  expect(offered).toBeGreaterThan(1);
  const picks = async (count: number) => {
    for (let i = 0; i < count; i += 1) {
      await stack.page.keyboard.press('ArrowDown');
      await stack.page.keyboard.press('Space');
    }
    await expect(stack.page.locator('.quick-input-widget .quick-input-count')).toContainText(
      `${count} Selected`,
    );
    await stack.page.keyboard.press('Enter');
  };
  await picks(1);
  const cancelDialog = dialog(stack.page, 'Approve 1 rule');
  await expect(cancelDialog).toBeVisible();
  await expect(cancelDialog).toContainText('Bound to test');
  await expect(cancelDialog).toContainText('new experiments only');
  await stack.shot('evolve-rules-confirm-approval');
  await cancelDialog.getByRole('button', { name: 'Cancel' }).click();

  const batch = (await stack.evolveCall('evolve_rules_propose', {
    training_source_id: richId,
  })) as {
    batch_id: string;
    idempotent: boolean;
  };
  expect(batch.idempotent).toBe(true);
  const stored = (await stack.evolveCall('evolve_rules_get', { batch_id: batch.batch_id })) as {
    test: {
      test_sha256: string;
      policy: Record<string, unknown>;
      evidence: { rule_sha256: string; decision: string }[];
    };
  };
  const supported = stored.test.evidence.filter((e) => e.decision === 'supported');
  expect(supported.length).toBeGreaterThan(1);
  expect(stored.test.policy).toMatchObject({ p_max: 0.05, alpha: 0.05 });

  // The bound text of one supported and one insufficient_evidence rule is in the document the person
  // just read, word for word as the service wrote it, next to the statement that it is not a signal.
  const withBound = stored.test.evidence as BoundEvidence[];
  const aSupported = withBound.find((e) => e.decision === 'supported')!;
  const anInsufficient = withBound.find((e) => e.decision === 'insufficient_evidence')!;
  expect(aSupported.bound_statement).toMatch(/held-out rows; with 95% confidence/);
  expect(anInsufficient.bound_statement).toMatch(/held-out rows; with 95% confidence/);
  expect(await documentContains(stack.page, aSupported.bound_statement!)).toBe(true);
  expect(await documentContains(stack.page, anInsufficient.bound_statement!)).toBe(true);
  expect(
    await documentContains(stack.page, 'upper bound (informational, not an approval signal)'),
  ).toBe(true);
  expect(
    await documentContains(
      stack.page,
      'a rule can have a low bound and still be insufficient evidence',
    ),
  ).toBe(true);
  await stack.shot('evolve-rules-bounds');
  // Proof that cancelling stored nothing: approving that one rule from outside is a FIRST approval.
  const probe = (await stack.evolveCall('evolve_rules_approve', {
    batch_id: batch.batch_id,
    rule_sha256s: [supported[0]!.rule_sha256],
    expected_test_sha256: stored.test.test_sha256,
    confirm: true,
  })) as { idempotent: boolean };
  expect(probe.idempotent).toBe(false);

  // 3. The real approval: choose the first two supported rules and confirm. The batch was tested once
  // (at 5%); choosing 1% now cannot redo it, and the person is told that their choice was not applied.
  confirmTest = await runReview(stack.page, richId, { pMax: '1', alpha: '5' });
  await confirmTest.getByRole('button', { name: 'Run confirmation test' }).click();
  await expect(notice(stack.page, /already tested[^]*was not applied/)).toBeVisible({
    timeout: 90_000 * SLOW,
  });
  await expect(
    stack.page
      .locator('.quick-input-widget')
      .getByPlaceholder(/Choose the supported rules to approve/),
  ).toBeVisible({ timeout: 90_000 * SLOW });
  await picks(2);
  const approveDialog = dialog(stack.page, 'Approve 2 rules');
  await expect(approveDialog).toBeVisible();
  const detail = await approveDialog.innerText();
  expect(detail).toContain(stored.test.test_sha256);
  await approveDialog.getByRole('button', { name: 'Approve rules' }).click();
  const approved = notice(stack.page, /Approved 2 rules\. Approved rule set SHA-256: [a-f0-9]{64}/);
  await expect(approved.first()).toBeVisible({ timeout: 60_000 * SLOW });
  const setHash = /SHA-256: ([a-f0-9]{64})/.exec(await approved.first().innerText())![1]!;
  await stack.shot('evolve-rules-approved');

  // Evolve recorded the set with alice as the approver.
  const set = (await stack.evolveCall('evolve_rules_get', { rule_set_sha256: setHash })) as {
    approver: string;
    rules: unknown[];
    data_sha256: string;
  };
  expect(set.approver).toBe('alice');
  expect(set.rules).toHaveLength(2);
  expect(set.data_sha256).toBe(rich.normalized_sha256);

  // The same two rules approved again from outside are an idempotent receipt: the person's click had
  // stored exactly this set (the UI lists supported rules in the report's order).
  const again = (await stack.evolveCall('evolve_rules_approve', {
    batch_id: batch.batch_id,
    rule_sha256s: supported.slice(0, 2).map((e) => e.rule_sha256),
    expected_test_sha256: stored.test.test_sha256,
    confirm: true,
  })) as { approved_rule_set_sha256: string; idempotent: boolean };
  expect(again.approved_rule_set_sha256).toBe(setHash);
  expect(again.idempotent).toBe(true);

  // 4. Attach it to a NEW experiment start: a sealed holdout and a start file, both the person's own.
  const experiment = `${id}-exp`;
  const zero = 'SELECT sku_id, 0::BIGINT AS order_qty FROM p1_observation';
  const per = (v: unknown) =>
    Object.fromEntries(Array.from({ length: 16 }, (_, i) => [String(i + 1), v]));
  const holdoutSkus = richSource(1007).skus.map((sku) => ({
    ...sku,
    history: [1],
    initial_pipeline: Array(90).fill(0),
  }));
  await stack.evolveCall('evolve_register_holdout', {
    holdout_id: `hold-${experiment}`,
    source_sha256: 'a'.repeat(64),
    training_source_id: richId,
    training_source_sha256: rich.normalized_sha256,
    source_ref: 'export:holdout',
    training_source_ref: 'export:training',
    training_start: '2026-08-01',
    training_end: '2026-10-29',
    history_start: '2026-11-30',
    history_end: '2026-11-30',
    inventory_as_of: '2026-12-01',
    holdout_start: '2026-12-01',
    holdout_end: '2027-02-28',
    outcomes_sealed_before_search: true,
    demand_observation: 'true_demand',
    problem: { capacity: 100000, skus: holdoutSkus },
    service_targets: { aggregate_min_fill_rate: 0.0, per_sku_min_fill_rate: per(0.0) },
    baseline_sql: zero,
    max_cost_ratio: 0.95,
    evaluator_version: 'replenishment_decision_v2',
    planning_window_days: 10,
    scored_window_days: 80,
    sensitivity_tail_days: [2, 10],
    unit_order_costs: per(1.0),
    terminal_stock_tolerance: per(0),
  });
  const startSpec = {
    pilot: 'p1_decision',
    experiment,
    brain: 'llm',
    evaluator_version: 'replenishment_decision_v2',
    seed_sql: zero,
    baseline_sql: zero,
    capacity: 100000,
    skus: richPayload.skus,
    service_targets: { aggregate_min_fill_rate: 0.0, per_sku_min_fill_rate: per(0.0) },
    planning_window_days: 10,
    scored_window_days: 80,
    unit_order_costs: per(1.0),
    terminal_stock_tolerance: per(0),
    holdout_id: `hold-${experiment}`,
    training_source_id: richId,
    source_sha256: rich.normalized_sha256,
    training_start: '2026-08-01',
    training_end: '2026-10-29',
    history_start: '2025-08-01',
    history_end: '2026-07-31',
    inventory_as_of: '2026-08-01',
    demand_observation: 'true_demand',
    max_generations: 0,
    budget: { max_evaluated: 1 },
  };
  const startFile = join(stack.workspaceDir, 'start-experiment.json');
  writeFileSync(startFile, JSON.stringify(startSpec));
  await stack.page.keyboard.press('Control+P');
  const quick = stack.page.locator('.quick-input-widget input');
  await quick.fill(startFile);
  await expect(stack.page.locator('.quick-input-list')).toContainText('start-experiment.json');
  await quick.press('Enter');
  await expect(stack.page.getByRole('tab', { name: 'start-experiment.json' })).toBeVisible();

  confirmTest = await runReview(stack.page, richId);
  await confirmTest.getByRole('button', { name: 'Run confirmation test' }).click();
  await expect(
    stack.page
      .locator('.quick-input-widget')
      .getByPlaceholder(/Choose the supported rules to approve/),
  ).toBeVisible({ timeout: 90_000 * SLOW });
  await picks(2);
  await dialog(stack.page, 'Approve 2 rules')
    .getByRole('button', { name: 'Approve rules' })
    .click();
  await notice(stack.page, 'Approved 2 rules')
    .first()
    .getByRole('button', {
      name: 'Start new experiment with these rules',
    })
    .click();
  await expect(
    stack.page.locator('.quick-input-widget').getByPlaceholder(/start file/),
  ).toBeVisible();
  await quick.fill('start-experiment');
  await quick.press('Enter');
  const startDialog = dialog(stack.page, `Start new experiment ${experiment}`);
  await expect(startDialog).toBeVisible();
  await expect(startDialog).toContainText(setHash);
  await expect(startDialog).toContainText('does not go through the Escurel plan review');
  await stack.shot('evolve-rules-start');
  await startDialog.getByRole('button', { name: 'Start experiment' }).click();
  await expect(
    notice(stack.page, `Experiment ${experiment} accepted by Evolve`).first(),
  ).toBeVisible({
    timeout: 120_000 * SLOW,
  });

  // The start carried exactly this hash: the same request with the hash is an idempotent receipt of
  // the experiment the window just started, and the same request without it is a different one.
  const replay = (await stack.evolveCall('evolve_start', {
    ...startSpec,
    approved_rule_set_sha256: setHash,
  })) as { idempotent?: boolean };
  expect(replay.idempotent).toBe(true);
  await expect(stack.evolveCall('evolve_start', startSpec)).rejects.toThrow(/HTTP (409|422|4\d\d)/);

  expect(
    stack.errors.filter(
      (e) =>
        !/favicon|ResizeObserver|DeprecationWarning|trace-deprecation|github\.copilot|EXTRA proposals/.test(
          e,
        ),
    ),
  ).toEqual([]);
});
