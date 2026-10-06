import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  fetchScenarioDiff,
  parseScenarioDiff,
  scenarioDiffSummary,
  scenarioDiffTexts,
} from '../../src/evolve/scenarioDiff';
import type { TokenRefresher } from '../../src/auth/refresher';

const body = {
  experiment: 'exp-1',
  pilot: 'p0',
  baseline_program_id: 1,
  winner_program_id: 2,
  tables: [{ table: 'p0_bin_assignment', rows_added: 0, rows_removed: 1, rows_modified: 2 }],
  rows: [
    {
      table: 'p0_bin_assignment',
      item_id: 2,
      change_type: 'modified',
      column_name: 'bin_id',
      old_value: '2',
      new_value: '1',
    },
    {
      table: 'p0_bin_assignment',
      item_id: 3,
      change_type: 'modified',
      column_name: 'bin_id',
      old_value: '3',
      new_value: '1',
    },
    {
      table: 'p0_bin_assignment',
      item_id: 4,
      change_type: 'removed',
      column_name: null,
      old_value: null,
      new_value: null,
    },
  ],
  truncated: false,
  diff_ref: 'scenario_diffs/exp-1.duckdb#winner',
  evidence_note: 'Search-time replay of the seed and the winner on the training instance.',
};

const refresher = (token: string | undefined): TokenRefresher =>
  ({
    get: async () => token,
    invalidate: async () => token,
  }) as unknown as TokenRefresher;

afterEach(() => vi.unstubAllGlobals());

describe('parseScenarioDiff', () => {
  it('accepts the Evolve response and keeps the evidence note', () => {
    const diff = parseScenarioDiff(body);
    expect(diff.winnerProgramId).toBe(2);
    expect(diff.tables[0]).toEqual({
      table: 'p0_bin_assignment',
      rowsAdded: 0,
      rowsRemoved: 1,
      rowsModified: 2,
    });
    expect(diff.evidenceNote).toContain('Search-time replay');
  });

  it('refuses a response that is not a scenario diff', () => {
    expect(() => parseScenarioDiff({ experiment: 'x' })).toThrow(/scenario diff/i);
    expect(() => parseScenarioDiff({ ...body, tables: 'nope' })).toThrow(/scenario diff/i);
    expect(() => parseScenarioDiff(null)).toThrow(/scenario diff/i);
  });
});

describe('scenarioDiffTexts', () => {
  it('renders both sides so the native diff shows only what changed', () => {
    const { seed, winner } = scenarioDiffTexts(parseScenarioDiff(body), 'p0_bin_assignment');
    expect(seed).toContain('item_id=2 · bin_id = 2');
    expect(winner).toContain('item_id=2 · bin_id = 1');
    // A removed row exists only on the seed side.
    expect(seed).toContain('item_id=4 · (row)');
    expect(winner).not.toContain('item_id=4');
  });

  it('is deterministic: the same diff renders the same text', () => {
    const diff = parseScenarioDiff(body);
    expect(scenarioDiffTexts(diff, 'p0_bin_assignment')).toEqual(
      scenarioDiffTexts(diff, 'p0_bin_assignment'),
    );
  });

  it('says when rows were cut, so a short diff is never mistaken for a complete one', () => {
    const { seed, winner } = scenarioDiffTexts(
      parseScenarioDiff({ ...body, truncated: true }),
      'p0_bin_assignment',
    );
    expect(seed).toMatch(/truncated/i);
    expect(winner).toMatch(/truncated/i);
  });
});

describe('scenarioDiffSummary', () => {
  it('uses exact counts and states what the diff is not', () => {
    const lines = scenarioDiffSummary(parseScenarioDiff(body));
    expect(lines.join('\n')).toContain('p0_bin_assignment');
    expect(lines.join('\n')).toContain('2 modified');
    expect(lines.join('\n')).toContain('1 removed');
    expect(lines.join('\n')).toMatch(/not independent validation/i);
  });
});

describe('fetchScenarioDiff', () => {
  it('calls the Evolve tool with the signed-in bearer and the experiment id', async () => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify(body), { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);
    const diff = await fetchScenarioDiff('http://127.0.0.1:8099', refresher('tok'), 'exp-1');
    expect(diff.experiment).toBe('exp-1');
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('http://127.0.0.1:8099/');
    expect((init.headers as Record<string, string>)['X-Triton-Tool']).toBe('evolve_scenario_diff');
    expect((init.headers as Record<string, string>).authorization).toBe('Bearer tok');
    expect(JSON.parse(init.body as string)).toEqual({ experiment: 'exp-1' });
  });

  it('refuses to call without a token instead of sending an anonymous request', async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    await expect(
      fetchScenarioDiff('http://127.0.0.1:8099', refresher(undefined), 'exp-1'),
    ).rejects.toThrow(/sign in/i);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('retries once with a fresh token after a 401', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response('{}', { status: 401 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(body), { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);
    const diff = await fetchScenarioDiff('http://127.0.0.1:8099', refresher('tok'), 'exp-1');
    expect(diff.winnerProgramId).toBe(2);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('turns an unsupported pilot into a message a person can act on', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(
        async () =>
          new Response(
            JSON.stringify({ error: '`evolve_scenario_diff` only supports pilot ...' }),
            {
              status: 422,
            },
          ),
      ),
    );
    await expect(
      fetchScenarioDiff('http://127.0.0.1:8099', refresher('tok'), 'exp-1'),
    ).rejects.toThrow(/does not support|not available/i);
  });

  it('rejects a non-loopback http endpoint', async () => {
    await expect(
      fetchScenarioDiff('http://evolve.example.com', refresher('tok'), 'exp-1'),
    ).rejects.toThrow(/HTTPS/);
  });
});
