import { describe, expect, it } from 'vitest';
import { parse } from 'yaml';
import { normalizeV2TrainingSpec, smokeOnlyWarnings, v2ProblemPage, v2TrainingStarter } from '../../src/evolve/problemImport';

const training = {
  capacity: 10,
  skus: [{ sku_id: 1, initial_stock: 1, initial_pipeline: [0, 0],
    history: [1, 1], demand: [1, 1], lead_time: 1, case_pack: 1,
    min_order: 0, holding_cost: 0, shortage_cost: 10, fixed_order_cost: 5 }],
  service_targets: { aggregate_min_fill_rate: 0.8, per_sku_min_fill_rate: { '1': 0.8 } },
  seed_sql: 'SELECT sku_id, 1::BIGINT AS order_qty FROM p1_observation',
  baseline_sql: 'SELECT sku_id, 1::BIGINT AS order_qty FROM p1_observation',
  planning_window_days: 2, scored_window_days: 2,
  unit_order_costs: { '1': 1 }, terminal_stock_tolerance: { '1': 0 },
  training_start: '2026-08-01', training_end: '2026-08-02',
  history_start: '2026-07-30', history_end: '2026-07-31',
  source_sha256: 'a'.repeat(64), max_generations: 0,
  budget: { max_evaluated: 1 },
};

describe('V2 problem import', () => {
  it('provides a training-only starter that requires a real source digest', () => {
    expect(v2TrainingStarter).not.toHaveProperty('holdout_id');
    expect(v2TrainingStarter).not.toHaveProperty('max_cost_ratio');
    expect(v2TrainingStarter.source_sha256).toMatch(/^REPLACE_/);
    expect(() => normalizeV2TrainingSpec(v2TrainingStarter, 'private-1')).toThrow(/source_sha256/);
    expect(smokeOnlyWarnings(v2TrainingStarter)).toEqual([
      'zero proposal generations', 'the seed and baseline are identical',
    ]);
  });
  it('creates an owner-scoped page with only training terms and a private holdout reference', () => {
    const page = v2ProblemPage({ id: 'reorder_test', owner: 'alice',
      holdoutId: 'private-1', objective: 'Reduce shortages', trainingSpec: training });
    expect(page.pageId).toBe('markdown/instances/evolve_problem/reorder_test.md');
    const front = parse(page.content.split('---\n')[1] ?? '');
    expect(front.owner_subject).toBe('alice');
    expect(front.search_request.evaluator_version).toBe('replenishment_decision_v2');
    expect(front.search_request.holdout_id).toBe('private-1');
    expect(front.search_request.skus[0].demand).toEqual([1, 1]);
    expect(page.content).not.toContain('holdout_demand');
    expect(training).not.toHaveProperty('holdout_id');
  });

  it('refuses private outcomes, acceptance terms, overrides, and incomplete contracts', () => {
    for (const key of ['holdout_id', 'holdout_demand', 'max_cost_ratio',
      'sensitivity_tail_days', 'pilot', 'evaluator_version']) {
      expect(() => normalizeV2TrainingSpec({ ...training, [key]: 'forbidden' }, 'private-1'))
        .toThrow(/Unsupported training-spec key/);
    }
    const { seed_sql: _seed, ...missing } = training;
    expect(() => normalizeV2TrainingSpec(missing, 'private-1')).toThrow(/seed_sql/);
    expect(() => normalizeV2TrainingSpec({ ...training,
      skus: [{ ...training.skus[0], holdout_demand: [9] }] }, 'private-1'))
      .toThrow(/Unsupported SKU 1 key holdout_demand/);
    expect(() => v2ProblemPage({ id: '../foreign', owner: 'alice',
      holdoutId: 'private-1', objective: '', trainingSpec: training })).toThrow(/page ID/);
  });
});
