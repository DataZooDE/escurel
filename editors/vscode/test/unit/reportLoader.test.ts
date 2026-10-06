import { describe, expect, it } from 'vitest';
import { loadReport } from '../../src/editors/reportLoader';

const REPORT = {
  id: 'exception-impact-report',
  title: 'Impact of a supplier delay',
  params: { supplier: { type: 'string' }, lot: { type: 'string' }, delay_days: { type: 'number' } },
  data: { totals: '[[query::delay_impact_summary]]', orders: '[[query::delay_impact]]' },
  views: [
    { kind: 'kpi', data: 'totals', agg: 'sum', field: 'orders_late', label: 'Orders late' },
    { kind: 'table', data: 'orders' },
  ],
};

function fakeClient(opts: { fm?: unknown; fail?: string } = {}) {
  const asked: Array<{ ref: string; params?: Record<string, unknown> }> = [];
  const client = {
    expand: async (r: { page_id: string }) => {
      if (r.page_id !== 'markdown/skills/exception-impact-report.md')
        throw new Error('unexpected page');
      return { frontmatter: opts.fm ?? REPORT };
    },
    queryInstance: async (r: { ref: string; params?: Record<string, unknown> }) => {
      asked.push(r);
      if (opts.fail && r.ref === opts.fail) throw new Error('boom');
      return r.ref === 'delay_impact_summary'
        ? { rows: [{ orders_late: 4 }] }
        : { rows: [{ customer_order: 'SO-1', days_late: 18 }] };
    },
  };
  return { client, asked };
}
const RECORD = { supplier: 'baltic-components', lot: 'L-24117', delay_days: 21 };

describe('loadReport', () => {
  it("runs each query of the report with the record's own values and builds the figures", async () => {
    const { client, asked } = fakeClient();
    const model = await loadReport(client, 'exception-impact-report', RECORD);
    expect(model?.title).toBe('Impact of a supplier delay');
    expect(model?.views[0]).toEqual({ kind: 'kpi', label: 'Orders late', value: '4' });
    expect(asked.map((a) => a.ref).sort()).toEqual(['delay_impact', 'delay_impact_summary']);
    for (const a of asked)
      expect(a.params).toEqual({ supplier: 'baltic-components', lot: 'L-24117', delay_days: 21 });
  });

  it('shows nothing, and asks nothing, when the record lacks a param of the report', async () => {
    const { client, asked } = fakeClient();
    expect(await loadReport(client, 'exception-impact-report', { supplier: 'x' })).toBeUndefined();
    expect(asked).toEqual([]);
  });

  it('shows nothing when one query fails, instead of half a figure', async () => {
    const { client } = fakeClient({ fail: 'delay_impact' });
    expect(await loadReport(client, 'exception-impact-report', RECORD)).toBeUndefined();
  });

  it('shows nothing when the page is not a report', async () => {
    const { client } = fakeClient({ fm: { title: 'x' } });
    expect(await loadReport(client, 'exception-impact-report', RECORD)).toBeUndefined();
  });
});
