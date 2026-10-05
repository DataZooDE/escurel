import { afterEach, describe, expect, it, vi } from 'vitest';
import type { TokenRefresher } from '../../src/auth/refresher';
import { callPrivateTrainingTool } from '../../src/evolve/trainingCsvClient';

afterEach(() => vi.unstubAllGlobals());

const refresher = (get = vi.fn(async () => 'owner-token'),
  invalidate = vi.fn(async () => 'refreshed-token')) =>
  ({ get, invalidate } as unknown as TokenRefresher);

describe('direct owner-private training CSV client', () => {
  it('sends the exact bytes under the bearer without following redirects', async () => {
    const body = { source_id: 'src_1', daily_demand_csv: 'PRIVATE_CSV_ROWS' };
    const fetcher = vi.fn(async (url: string, init: RequestInit) => {
      expect(url).toBe('https://evolve.example/');
      expect(init.redirect).toBe('error');
      expect((init.headers as Record<string, string>).authorization).toBe('Bearer owner-token');
      expect((init.headers as Record<string, string>)['X-Triton-Tool']).toBe('evolve_prepare_training_csv');
      expect(JSON.parse(init.body as string)).toEqual(body);
      return { ok: true, status: 200, json: async () => ({ training_source_id: 'src_1' }) } as Response;
    });
    vi.stubGlobal('fetch', fetcher);
    expect(await callPrivateTrainingTool('https://evolve.example', refresher(),
      'evolve_prepare_training_csv', body)).toEqual({ training_source_id: 'src_1' });
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it('refreshes once after a 401 and retries identical input after an uncertain response', async () => {
    const bodies: string[] = [];
    const tokens: string[] = [];
    const fetcher = vi.fn(async (_url: string, init: RequestInit) => {
      bodies.push(init.body as string);
      tokens.push(String((init.headers as Record<string, string>).authorization));
      if (bodies.length === 1) return { ok: false, status: 401 } as Response;
      if (bodies.length === 2) throw new Error('connection lost after commit');
      return { ok: true, status: 200, json: async () => ({ idempotent: true }) } as Response;
    });
    vi.stubGlobal('fetch', fetcher);
    const get = vi.fn().mockResolvedValueOnce('owner-token').mockResolvedValue('refreshed-token');
    const invalidate = vi.fn(async () => 'refreshed-token');
    expect(await callPrivateTrainingTool('https://evolve.example', refresher(get, invalidate),
      'evolve_prepare_training_csv', { source_id: 'src_1' })).toEqual({ idempotent: true });
    expect(invalidate).toHaveBeenCalledTimes(1);
    expect(tokens).toEqual(['Bearer owner-token', 'Bearer refreshed-token', 'Bearer refreshed-token']);
    expect(new Set(bodies).size).toBe(1);
  });

  it('keeps private server error bodies out of recovery guidance', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => ({ ok: false, status: 409,
      json: async () => ({ error: 'PRIVATE_CSV_ROWS' }) } as Response)));
    await expect(callPrivateTrainingTool('https://evolve.example', refresher(),
      'evolve_prepare_training_csv', { source_id: 'src_1' }))
      .rejects.toThrow(/new ID/);
    await expect(callPrivateTrainingTool('https://evolve.example', refresher(),
      'evolve_prepare_training_csv', { source_id: 'src_1' }))
      .rejects.not.toThrow(/PRIVATE_CSV_ROWS/);
  });
});
