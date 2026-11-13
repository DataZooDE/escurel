import { describe, expect, it } from 'vitest';
import { EscurelError } from '../../src/client/errors';
import type { UsageReportResponse } from '../../src/client/types';
import {
  USAGE_GROUPS,
  USAGE_WINDOWS,
  usageFailure,
  usageRange,
  usageRows,
  usageTitle,
} from '../../src/views/usageModel';

// The wire is `usage_report` (references/02): rows of {group, runs, by_status, input_tokens,
// output_tokens, cost_usd} plus a total; success rate is by_status.processed / runs.
const report: UsageReportResponse = {
  group_by: ['skill'],
  since: '2026-11-06T06:00:00.000Z',
  until: '2026-11-13T06:00:00.000Z',
  rows: [
    {
      group: { skill: 'triage' },
      runs: 2,
      by_status: { processed: 2 },
      input_tokens: 800,
      output_tokens: 100,
      cost_usd: 0.004,
    },
    {
      group: { skill: 'supplier-risk' },
      runs: 4,
      by_status: { processed: 3, dead_letter: 1 },
      input_tokens: 12_000,
      output_tokens: 2_400,
      cost_usd: 0.12,
    },
  ],
  total: {
    runs: 6,
    by_status: { processed: 5, dead_letter: 1 },
    input_tokens: 12_800,
    output_tokens: 2_500,
    cost_usd: 0.124,
  },
};

describe('usage view model', () => {
  it('offers three windows and the five groupings the gateway knows', () => {
    expect(USAGE_WINDOWS.map((w) => w.id)).toEqual(['24h', '7d', '30d']);
    expect(USAGE_GROUPS.map((g) => g.id)).toEqual(['skill', 'model', 'agent', 'day', 'page']);
  });

  it('turns a window into the RFC 3339 range usage_report reads', () => {
    const now = new Date('2026-11-13T06:00:00.000Z');
    expect(usageRange('24h', now)).toEqual({
      since: '2026-11-12T06:00:00.000Z',
      until: '2026-11-13T06:00:00.000Z',
    });
    expect(usageRange('30d', now).since).toBe('2026-10-14T06:00:00.000Z');
  });

  it('puts the total first, then the groups by cost', () => {
    const rows = usageRows(report, 'skill');
    expect(rows.map((r) => r.label)).toEqual(['All runs', 'supplier-risk', 'triage']);
    expect(rows[0]).toMatchObject({
      kind: 'total',
      description: '6 runs · 83% ok · 15.3k tokens · $0.12',
    });
    expect(rows[1]!.description).toBe('4 runs · 75% ok · 14.4k tokens · $0.12');
    expect(rows[1]!.tooltip).toContain('done 3 · gave up 1');
    expect(rows[1]!.tooltip).toContain('12,000 input tokens · 2,400 output tokens');
  });

  it('names agents by their id, pages by their slug, and an empty group plainly', () => {
    const row = (group: UsageReportResponse['rows'][number]['group']) => ({
      ...report.rows[0]!,
      group,
    });
    const by = (
      dim: 'agent' | 'page' | 'model',
      group: UsageReportResponse['rows'][number]['group'],
    ) => usageRows({ ...report, group_by: [dim], rows: [row(group)] }, dim)[1]!.label;
    expect(by('agent', { agent: 'agent:scribe' })).toBe('scribe');
    expect(by('page', { page: 'markdown/instances/order/o1.md' })).toBe('o1');
    expect(by('model', { model: null })).toBe('(not reported)');
  });

  it('sorts days newest first, not by cost', () => {
    const days = usageRows(
      {
        ...report,
        group_by: ['day'],
        rows: [
          { ...report.rows[1]!, group: { day: '2026-11-11' } },
          { ...report.rows[0]!, group: { day: '2026-11-12' } },
        ],
      },
      'day',
    );
    expect(days.map((r) => r.label)).toEqual(['All runs', '2026-11-12', '2026-11-11']);
  });

  it('shows no rows when nothing finished in the window', () => {
    expect(
      usageRows({ ...report, rows: [], total: { ...report.total, runs: 0, cost_usd: 0 } }, 'skill'),
    ).toEqual([]);
  });

  it('says what it shows, and that a non-admin needs an admin sign-in', () => {
    expect(usageTitle('7d', 'skill')).toBe('Last 7 days, by skill');
    expect(
      usageFailure(new EscurelError('admin_required', 'admin role required for this tool')),
    ).toEqual({ adminRequired: true, message: 'Usage needs an admin sign-in.' });
    expect(usageFailure(new Error('socket hang up'))).toEqual({
      adminRequired: false,
      message: 'Could not read usage: socket hang up',
    });
  });
});
