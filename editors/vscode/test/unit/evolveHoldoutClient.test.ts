import { afterEach, describe, expect, it, vi } from 'vitest';
import { EvolveRegistrationError, evolveOrigin, prepareHoldout, registerHoldoutAtEvolve, requireReviewedFileBytes } from '../../src/evolve/holdoutClient';
import type { TokenRefresher } from '../../src/auth/refresher';

const source = { sourceId: 'src_example', digest: 'a'.repeat(64),
  trainingStart: '2026-08-01', trainingEnd: '2026-08-06' };
const fixture = {
  holdout_id: 'synthetic-case-1', evaluator_version: 'replenishment_decision_v2',
  training_source_id: 'REPLACE_WITH_SOURCE', training_source_sha256: 'REPLACE_WITH_DIGEST',
  source_ref: 'synthetic:published', source_sha256: 'b'.repeat(64),
  training_start: '2026-08-01', training_end: '2026-08-06',
  history_start: '2026-08-30', history_end: '2026-08-31',
  holdout_start: '2026-09-01', holdout_end: '2026-09-06', inventory_as_of: '2026-09-01',
  outcomes_sealed_before_search: true, outcomes_publicly_disclosed: true,
  demand_observation: 'true_demand', baseline_sql: 'SELECT 1', max_cost_ratio: 0.9,
  service_targets: { aggregate_min_fill_rate: 0.8, per_sku_min_fill_rate: { '1': 0.8 } },
  unit_order_costs: { '1': 1 }, terminal_stock_tolerance: { '1': 0 },
  planning_window_days: 2, scored_window_days: 2, sensitivity_tail_days: [2, 4],
  problem: { capacity: 10, skus: [{ sku_id: 1, initial_stock: 1, initial_pipeline: [0, 0],
    demand: [987654321, 123456789], holding_cost: 1, fixed_order_cost: 2, shortage_cost: 10 }] },
};

afterEach(() => vi.unstubAllGlobals());

describe('direct private V2 holdout intake', () => {
  it('rejects insecure or redirectable endpoint configurations', () => {
    expect(evolveOrigin('https://evolve.example')).toBe('https://evolve.example');
    expect(evolveOrigin('http://127.0.0.1:8844')).toBe('http://127.0.0.1:8844');
    expect(() => evolveOrigin('http://evolve.example')).toThrow(/HTTPS origin/);
    expect(() => evolveOrigin('https://evolve.example/redirect')).toThrow(/HTTPS origin/);
    expect(() => evolveOrigin('https://user:secret@evolve.example')).toThrow(/HTTPS origin/);
  });

  it('summarizes the acceptance contract without exposing realized demand', () => {
    const { payload, summary } = prepareHoldout(fixture, source);
    expect(payload.training_source_id).toBe(source.sourceId);
    expect(payload.training_source_sha256).toBe(source.digest);
    expect(prepareHoldout(fixture, { ...source, sourceId: 'owner-training-source' }).payload.training_source_id)
      .toBe('owner-training-source');
    expect(summary).toContain('Publicly disclosed synthetic fixture');
    expect(summary).toContain('Maximum cost ratio: 0.9');
    expect(summary).toContain('opening_stock');
    expect(summary).toContain('Baseline rule preview: SELECT 1');
    expect(summary).not.toContain('987654321');
    expect(summary).not.toContain('123456789');
    expect(() => prepareHoldout({ ...fixture, outcomes_publicly_disclosed: undefined }, source))
      .toThrow(/Explicitly set/);
    expect(() => prepareHoldout({ ...fixture, training_source_id: 'src_other' }, source))
      .toThrow(/different training source/);
  });

  it('refuses to seal saved bytes while the visible editor has changed', () => {
    const saved = new TextEncoder().encode('{"holdout_id":"one"}');
    expect(() => requireReviewedFileBytes(saved, '{"holdout_id":"two"}', true))
      .toThrow(/unsaved changes/);
    expect(() => requireReviewedFileBytes(saved, '{"holdout_id":"two"}', false))
      .toThrow(/unsaved changes/);
    expect(() => requireReviewedFileBytes(saved, '{"holdout_id":"one"}', false))
      .not.toThrow();
  });

  it('sends the frozen declaration directly with a bearer, refuses redirects, and accepts an idempotent receipt', async () => {
    const { payload } = prepareHoldout(fixture, source);
    const fetcher = vi.fn(async (_url: string, init: RequestInit) => {
      expect(init.redirect).toBe('error');
      expect((init.headers as Record<string, string>).authorization).toBe('Bearer owner-token');
      expect((init.headers as Record<string, string>)['X-Triton-Tool']).toBe('evolve_register_holdout');
      expect(JSON.parse(init.body as string)).toEqual(payload);
      return { ok: true, status: 200,
        json: async () => ({ holdout_id: payload.holdout_id,
          holdout_sha256: 'c'.repeat(64), state: 'sealed', idempotent: true }) } as Response;
    });
    vi.stubGlobal('fetch', fetcher);
    const refresher = { get: async () => 'owner-token', invalidate: async () => 'owner-token' } as TokenRefresher;
    const receipt = await registerHoldoutAtEvolve('https://evolve.example', refresher, payload);
    expect(receipt).toEqual({ holdoutId: fixture.holdout_id, holdoutSha256: 'c'.repeat(64) });
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it('retries a transient response with the identical body and treats an ID conflict as final', async () => {
    const { payload } = prepareHoldout(fixture, source);
    const bodies: string[] = [];
    vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
      bodies.push(init.body as string);
      if (bodies.length === 1) throw new Error('connection lost after server commit');
      return { ok: true, status: 200,
        json: async () => ({ holdout_id: payload.holdout_id,
          holdout_sha256: 'c'.repeat(64), state: 'sealed', idempotent: true }) } as Response;
    }));
    const refresher = { get: async () => 'owner-token', invalidate: async () => 'owner-token' } as TokenRefresher;
    await registerHoldoutAtEvolve('https://evolve.example', refresher, payload);
    expect(bodies).toHaveLength(2);
    expect(bodies[0]).toBe(bodies[1]);
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: false, status: 409 } as Response)));
    await expect(registerHoldoutAtEvolve('https://evolve.example', refresher, payload))
      .rejects.toThrow(/already sealed with a different declaration/);
  });

  it('refreshes a rejected bearer once and gives data-safe binding guidance', async () => {
    const { payload } = prepareHoldout(fixture, source);
    let calls = 0;
    const invalidate = vi.fn(async () => 'refreshed-token');
    const refresher = { get: async () => 'old-token', invalidate } as unknown as TokenRefresher;
    vi.stubGlobal('fetch', vi.fn(async (_url: string, init: RequestInit) => {
      calls += 1;
      if (calls === 1) return { ok: false, status: 401 } as Response;
      expect((init.headers as Record<string, string>).authorization).toBe('Bearer refreshed-token');
      return { ok: false, status: 422,
        json: async () => ({ reason: 'invalid_training_source_binding',
          error: 'private-demand-row-must-never-appear' }) } as Response;
    }));
    const error = await registerHoldoutAtEvolve('https://evolve.example', refresher, payload)
      .catch((caught: unknown) => caught);
    expect(error).toBeInstanceOf(EvolveRegistrationError);
    expect(error).toMatchObject({ conclusiveNoStore: true });
    expect((error as Error).message).toMatch(/source ID, normalized digest, training dates/);
    expect((error as Error).message).not.toContain('private-demand-row-must-never-appear');
    expect(invalidate).toHaveBeenCalledOnce();
    expect(calls).toBe(2);
  });
});
