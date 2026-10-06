import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  comparisonSummary,
  evolveReachable,
  tableSummary,
  comparisonTexts,
  ComparisonNotFoundError,
  fetchComparison,
  matchesPage,
  parseComparison,
} from '../../src/evolve/scenarioDiff';
import type { TokenRefresher } from '../../src/auth/refresher';

const HASH = 'd'.repeat(64);
const body = {
  comparison: 'cmp-1',
  experiment: 'exp-1',
  state: 'completed',
  reason: null,
  baseline_program_id: 2,
  candidate_program_id: 3,
  baseline_code_sha256: 'b'.repeat(64),
  candidate_code_sha256: 'c'.repeat(64),
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
  next_cursor: null,
  truncated: false,
  evidence_note: 'Search-time replay of the baseline and the candidate on the training instance.',
  result_sha256: HASH,
};

const refresher = (token: string | undefined): TokenRefresher =>
  ({ get: async () => token, invalidate: async () => token }) as unknown as TokenRefresher;

afterEach(() => vi.unstubAllGlobals());

describe('parseComparison', () => {
  it('accepts Evolve’s comparison and keeps the hash and evidence note', () => {
    const c = parseComparison(body);
    expect(c).toMatchObject({
      comparison: 'cmp-1',
      experiment: 'exp-1',
      state: 'completed',
      candidateProgramId: 3,
      resultSha256: HASH,
    });
    expect(c.tables[0]).toEqual({
      table: 'p0_bin_assignment',
      rowsAdded: 0,
      rowsRemoved: 1,
      rowsModified: 2,
    });
    expect(c.evidenceNote).toContain('Search-time replay');
  });

  it('accepts a blocked comparison with its reason', () => {
    const c = parseComparison({
      ...body,
      state: 'blocked',
      reason: 'no scenario state tables',
      tables: [],
      rows: [],
    });
    expect(c.state).toBe('blocked');
    expect(c.reason).toContain('no scenario');
  });

  it('refuses a response that is not a comparison', () => {
    expect(() => parseComparison({ experiment: 'x' })).toThrow(/comparison/i);
    expect(() => parseComparison({ ...body, tables: 'nope' })).toThrow(/comparison/i);
    expect(() => parseComparison({ ...body, state: 'weird' })).toThrow(/comparison/i);
    expect(() => parseComparison(null)).toThrow(/comparison/i);
  });
});

describe('matchesPage', () => {
  it('trusts a comparison only when the page carries the same result hash', () => {
    const c = parseComparison(body);
    expect(matchesPage(c, HASH)).toBe(true);
    expect(matchesPage(c, 'e'.repeat(64))).toBe(false);
    expect(matchesPage(c, undefined)).toBe(false);
    // A forged "completed" page with no hash cannot vouch for anything.
    expect(matchesPage(c, '')).toBe(false);
  });
});

describe('comparisonTexts', () => {
  it('renders both sides so the native diff shows only what changed', () => {
    const { baseline, candidate } = comparisonTexts(parseComparison(body), 'p0_bin_assignment');
    expect(baseline).toContain('item_id=2 · bin_id = 2');
    expect(candidate).toContain('item_id=2 · bin_id = 1');
    expect(baseline).toContain('item_id=4 · (row)');
    expect(candidate).not.toContain('item_id=4');
    expect(baseline).toContain('program 2');
    expect(candidate).toContain('program 3');
    // A row that did not change is absent on both sides; the header says so.
    expect(baseline.split('\n')[0]).toContain('changed rows only');
    expect(candidate.split('\n')[0]).toContain('changed rows only');
  });

  it('is deterministic', () => {
    const c = parseComparison(body);
    expect(comparisonTexts(c, 'p0_bin_assignment')).toEqual(
      comparisonTexts(c, 'p0_bin_assignment'),
    );
  });

  it('says when rows were cut, so a short diff is never mistaken for a complete one', () => {
    const { baseline, candidate } = comparisonTexts(
      parseComparison({ ...body, truncated: true }),
      'p0_bin_assignment',
    );
    expect(baseline).toMatch(/truncated/i);
    expect(candidate).toMatch(/truncated/i);
  });
});

describe('comparisonSummary', () => {
  it('uses exact counts and states what the comparison is not', () => {
    const text = comparisonSummary(parseComparison(body)).join('\n');
    expect(text).toContain('p0_bin_assignment');
    expect(text).toContain('2 modified');
    expect(text).toContain('1 removed');
    expect(text).toMatch(/not independent validation/i);
  });
});

describe('fetchComparison', () => {
  it('calls the Evolve tool with the signed-in bearer and the comparison id', async () => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify(body), { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);
    const c = await fetchComparison('http://127.0.0.1:8099', refresher('tok'), 'cmp-1');
    expect(c.comparison).toBe('cmp-1');
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('http://127.0.0.1:8099/');
    const headers = init.headers as Record<string, string>;
    expect(headers['X-Triton-Tool']).toBe('evolve_comparison');
    expect(headers.authorization).toBe('Bearer tok');
    expect(JSON.parse(init.body as string)).toEqual({ comparison: 'cmp-1' });
  });

  it('follows the cursor until every stored row is read', async () => {
    const first = { ...body, rows: body.rows.slice(0, 2), next_cursor: '2' };
    const second = { ...body, rows: body.rows.slice(2), next_cursor: null };
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response(JSON.stringify(first), { status: 200 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(second), { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);
    const c = await fetchComparison('http://127.0.0.1:8099', refresher('tok'), 'cmp-1');
    expect(c.rows).toHaveLength(3);
    expect(
      JSON.parse((fetchMock.mock.calls[1] as unknown as [string, RequestInit])[1].body as string),
    ).toEqual({
      comparison: 'cmp-1',
      cursor: '2',
    });
  });

  it('refuses to call without a token instead of sending an anonymous request', async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    await expect(
      fetchComparison('http://127.0.0.1:8099', refresher(undefined), 'cmp-1'),
    ).rejects.toThrow(/sign in/i);
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it('retries once with a fresh token after a 401', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response('{}', { status: 401 }))
      .mockResolvedValueOnce(new Response(JSON.stringify(body), { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);
    const c = await fetchComparison('http://127.0.0.1:8099', refresher('tok'), 'cmp-1');
    expect(c.candidateProgramId).toBe(3);
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it('says "not found" the same way for a missing and a foreign comparison', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('{}', { status: 404 })),
    );
    await expect(
      fetchComparison('http://127.0.0.1:8099', refresher('tok'), 'cmp-1'),
    ).rejects.toBeInstanceOf(ComparisonNotFoundError);
  });

  it('rejects a non-loopback http endpoint', async () => {
    await expect(
      fetchComparison('http://evolve.example.com', refresher('tok'), 'cmp-1'),
    ).rejects.toThrow(/HTTPS/);
  });
});

describe('crew review fixes', () => {
  it('never prints a literal null: an empty cell reads as (empty)', () => {
    const c = parseComparison({
      ...body,
      rows: [
        {
          table: 'p0_bin_assignment',
          item_id: 2,
          change_type: 'modified',
          column_name: 'bin_id',
          old_value: null,
          new_value: '1',
        },
      ],
    });
    const { baseline, candidate } = comparisonTexts(c, 'p0_bin_assignment');
    expect(baseline).toContain('bin_id = (empty)');
    expect(baseline).not.toContain('null');
    expect(candidate).toContain('bin_id = 1');
  });

  it('marks the comparison truncated when stored rows remain after the page cap', async () => {
    const page = (n: number) =>
      new Response(
        JSON.stringify({
          ...body,
          rows: body.rows.slice(0, 1),
          next_cursor: String(n + 1),
          truncated: false,
        }),
        { status: 200 },
      );
    let calls = 0;
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => page(calls++)),
    );
    const c = await fetchComparison('http://127.0.0.1:8099', refresher('tok'), 'cmp-1');
    expect(c.truncated).toBe(true);
  });

  it('summarises the table that was opened, not the disclaimer', () => {
    const c = parseComparison(body);
    expect(tableSummary(c, 'p0_bin_assignment')).toBe(
      'p0_bin_assignment: 2 modified, 0 added, 1 removed',
    );
    expect(tableSummary(c, 'missing')).toBe('missing: no changes');
  });
});

describe('evolveReachable', () => {
  it('is true when the service answers its health check, without sending a token', async () => {
    const fetchMock = vi.fn(async () => new Response('ok', { status: 200 }));
    vi.stubGlobal('fetch', fetchMock);
    expect(await evolveReachable('http://127.0.0.1:8099')).toBe(true);
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('http://127.0.0.1:8099/healthz');
    expect(JSON.stringify(init)).not.toMatch(/authorization/i);
  });

  it('is false when the service is down or answers an error', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('ECONNREFUSED');
      }),
    );
    expect(await evolveReachable('http://127.0.0.1:8099')).toBe(false);
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('', { status: 503 })),
    );
    expect(await evolveReachable('http://127.0.0.1:8099')).toBe(false);
  });

  it('is false for an endpoint that is not allowed, instead of calling it', async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal('fetch', fetchMock);
    expect(await evolveReachable('http://evolve.example.com')).toBe(false);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
