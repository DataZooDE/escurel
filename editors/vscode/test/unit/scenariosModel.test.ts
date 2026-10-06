import { describe, expect, it } from 'vitest';
import {
  SCENARIO_SCHEME,
  comparisonPageRows,
  comparisonRequestPage,
  collectInstances,
  comparisonId,
  comparisonUri,
  endpointProblem,
  verifiedBlockedLabel,
  shouldPoll,
  parseComparisonUri,
  tableRows,
} from '../../src/views/scenariosModel';
import { parseComparison } from '../../src/evolve/scenarioDiff';

const HASH = 'd'.repeat(64);
const instance = (id: string, frontmatter: Record<string, unknown>) => ({
  page_id: `markdown/instances/evolve_comparison/${id}.md`,
  skill: 'evolve_comparison',
  at: '2026-10-01T00:00:00Z',
  frontmatter,
});

describe('comparisonPageRows', () => {
  const rows = comparisonPageRows([
    instance('cmp-b', { experiment: 'exp-2', status: 'requested' }),
    instance('cmp-a', { experiment: 'exp-1', status: 'completed', result_sha256: HASH }),
    instance('cmp-c', {
      experiment: 'exp-3',
      status: 'blocked',
      reason: 'no scenario state tables',
    }),
    instance('odd', {}),
  ]);

  it('lists comparisons by id with the experiment and state', () => {
    expect(rows.map((r) => r.comparison)).toEqual(['cmp-a', 'cmp-b', 'cmp-c', 'odd']);
    expect(rows[0]).toMatchObject({
      kind: 'comparison',
      status: 'completed',
      description: 'completed · exp-1',
    });
  });

  it('keeps what the page claims so the view can check it against Evolve', () => {
    expect(rows[0]?.pageResultSha256).toBe(HASH);
    expect(rows[1]?.pageResultSha256).toBeUndefined();
    expect(rows[2]?.reason).toBe('no scenario state tables');
  });

  it('treats a page with no recognizable status as unknown, never as completed', () => {
    expect(rows[3]?.status).toBe('unknown');
  });
});

describe('tableRows', () => {
  const result = parseComparison({
    comparison: 'cmp-a',
    experiment: 'exp-1',
    state: 'completed',
    baseline_program_id: 2,
    candidate_program_id: 3,
    tables: [
      { table: 'p0_bin_assignment', rows_added: 0, rows_removed: 1, rows_modified: 2 },
      { table: 'a_table', rows_added: 3, rows_removed: 0, rows_modified: 0 },
    ],
    rows: [],
    truncated: false,
    evidence_note: '',
    result_sha256: HASH,
  });

  it('lists tables with exact counts, sorted by name', () => {
    const rows = tableRows(result, true);
    expect(rows.map((r) => (r.kind === 'table' ? r.table : ''))).toEqual([
      'a_table',
      'p0_bin_assignment',
    ]);
    expect(rows.map((r) => r.description)).toEqual(['3 added', '2 modified · 1 removed']);
  });

  it('shows nothing from Evolve when the page does not match its record', () => {
    expect(tableRows(result, false)).toEqual([
      {
        kind: 'unverified',
        label: 'Unverified: this page does not match Evolve’s record',
        description: '',
      },
    ]);
  });

  it('says so when the candidate changed nothing', () => {
    const empty = parseComparison({
      comparison: 'x',
      experiment: 'e',
      state: 'completed',
      candidate_program_id: 2,
      tables: [],
      rows: [],
    });
    expect(tableRows(empty, true)).toEqual([
      { kind: 'empty', label: 'No differences from the baseline', description: '' },
    ]);
  });
});

describe('comparison URIs', () => {
  it('round-trips a comparison, table and side', () => {
    const uri = comparisonUri('cmp-a', 'p0_bin_assignment', 'candidate');
    expect(uri.startsWith(`${SCENARIO_SCHEME}:`)).toBe(true);
    expect(parseComparisonUri(uri)).toEqual({
      comparison: 'cmp-a',
      table: 'p0_bin_assignment',
      side: 'candidate',
    });
  });

  it('refuses a uri it did not make', () => {
    expect(parseComparisonUri('file:///etc/passwd')).toBeUndefined();
    expect(parseComparisonUri(`${SCENARIO_SCHEME}:/a/b/c/d`)).toBeUndefined();
    expect(parseComparisonUri(`${SCENARIO_SCHEME}:/a/t/middle`)).toBeUndefined();
  });
});

describe('comparisonRequestPage', () => {
  it('writes an owner-private request that is waiting to be computed', () => {
    const page = comparisonRequestPage({
      id: 'cmp-1',
      owner: 'alice',
      experiment: 'exp-1',
      baseline: 'parent',
    });
    expect(page).toContain('skill: evolve_comparison');
    expect(page).toContain('id: cmp-1');
    expect(page).toContain('owner_subject: "alice"');
    expect(page).toContain('experiment: exp-1');
    expect(page).toContain('baseline: parent');
    expect(page).toContain('candidate: winner');
    expect(page).toContain('status: requested');
    expect(page).toContain('next_comparison_action: evolve_compare');
  });

  it('quotes the owner so an odd subject cannot add frontmatter keys', () => {
    const page = comparisonRequestPage({
      id: 'cmp-1',
      owner: 'a"\nstatus: completed',
      experiment: 'exp-1',
      baseline: 'seed',
    });
    expect(page.match(/^status:/gm)).toHaveLength(1);
  });

  it('refuses ids, experiments and baselines that are not plain tokens', () => {
    const ok = { id: 'cmp-1', owner: 'alice', experiment: 'exp-1', baseline: 'seed' };
    expect(() => comparisonRequestPage({ ...ok, id: '../x' })).toThrow(/comparison/);
    expect(() => comparisonRequestPage({ ...ok, experiment: 'a b' })).toThrow(/experiment/);
    expect(() => comparisonRequestPage({ ...ok, baseline: 'seed\nstatus: completed' })).toThrow(
      /baseline/,
    );
    expect(() => comparisonRequestPage({ ...ok, owner: '' })).toThrow(/owner/);
  });
});

describe('shouldPoll', () => {
  const rows = (...statuses: ('requested' | 'completed' | 'blocked' | 'unknown')[]) =>
    comparisonPageRows(statuses.map((status, i) => instance(`c${i}`, { experiment: 'e', status })));

  it('keeps looking only while a comparison is waiting for Evolve and the view is open', () => {
    expect(shouldPoll(rows('requested', 'completed'), true)).toBe(true);
    expect(shouldPoll(rows('completed', 'blocked'), true)).toBe(false);
    expect(shouldPoll(rows(), true)).toBe(false);
  });

  it('never polls a view nobody is looking at', () => {
    expect(shouldPoll(rows('requested'), false)).toBe(false);
  });
});

describe('comparisonId', () => {
  it('keeps the unique suffix even for a very long experiment id', () => {
    const long = 'e'.repeat(128);
    const a = comparisonId(long, 'seed', 1_000);
    const b = comparisonId(long, 'parent', 2_000);
    expect(a.length).toBeLessThanOrEqual(128);
    expect(a).not.toBe(b);
    expect(a).toMatch(/-seed-/);
    expect(a.endsWith((1_000).toString(36))).toBe(true);
  });

  it('is a plain token for ordinary input', () => {
    expect(comparisonId('exp-1', 'parent', 5)).toBe(`cmp-exp-1-parent-${(5).toString(36)}`);
    expect(comparisonId('exp-1', '42', 5)).toMatch(/^cmp-exp-1-42-/);
  });
});

describe('endpointProblem', () => {
  it('is undefined for an allowed endpoint and explains a bad one', () => {
    expect(endpointProblem('http://127.0.0.1:8099')).toBeUndefined();
    expect(endpointProblem('https://evolve.example.com')).toBeUndefined();
    expect(endpointProblem('http://evolve.example.com')).toMatch(/HTTPS/);
    expect(endpointProblem('not a url')).toBeTruthy();
    expect(endpointProblem('')).toMatch(/escurel.evolveEndpoint/);
  });
});

describe('collectInstances', () => {
  const page = (...ids: string[]) => ({
    instances: ids.map((id) => instance(id, { experiment: 'e', status: 'requested' })),
    next_cursor: null,
  });

  it('follows every page, so the 101st comparison is reachable', async () => {
    async function* pages() {
      yield page(...Array.from({ length: 100 }, (_, i) => `c${i}`));
      yield page('c100');
    }
    const all = await collectInstances(pages(), 1000);
    expect(all).toHaveLength(101);
    expect(all.at(-1)?.page_id).toContain('c100');
  });

  it('stops at the cap rather than reading without limit', async () => {
    async function* endless() {
      for (;;) yield page('a', 'b', 'c');
    }
    expect((await collectInstances(endless(), 7)).length).toBe(7);
  });
});

describe('verifiedBlockedLabel', () => {
  it('shows the reason from Evolve’s record, never the page’s own claim', () => {
    const record = parseComparison({
      comparison: 'c',
      experiment: 'e',
      state: 'blocked',
      reason: 'no scenario state tables',
      tables: [],
      rows: [],
    });
    expect(verifiedBlockedLabel(record)).toBe('Blocked: no scenario state tables');
    const completed = parseComparison({
      comparison: 'c',
      experiment: 'e',
      state: 'completed',
      tables: [],
      rows: [],
    });
    expect(verifiedBlockedLabel(completed)).toBeUndefined();
  });
});
