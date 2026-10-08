import { describe, expect, it } from 'vitest';
import { buildReport, parseReport, reportParams } from '../../src/shared/report';

// A report skill (Peacock) says what to draw: params, named data (queries) and views. The workbench
// shows what it can with its own widgets: KPI figures and tables. Charts are not drawn here.
const IMPACT = {
  title: 'Impact of a supplier delay',
  params: { supplier: { type: 'string' }, lot: { type: 'string' }, delay_days: { type: 'number' } },
  data: { totals: '[[query::delay_impact_summary]]', orders: '[[query::delay_impact]]' },
  views: [
    { kind: 'kpi', data: 'totals', agg: 'sum', field: 'orders_late', label: 'Orders late' },
    {
      kind: 'kpi',
      data: 'totals',
      agg: 'sum',
      field: 'penalty_exposure_eur',
      label: 'Penalty exposure (EUR)',
    },
    { kind: 'vega', data: 'orders', spec: 'days_late' },
    { kind: 'table', data: 'orders' },
  ],
};

describe('parseReport', () => {
  it('reads the params, the queries and the views of a report skill page', () => {
    const def = parseReport(IMPACT)!;
    expect(def.title).toBe('Impact of a supplier delay');
    expect(def.params).toEqual([
      { name: 'supplier', type: 'string' },
      { name: 'lot', type: 'string' },
      { name: 'delay_days', type: 'number' },
    ]);
    expect(def.queries).toEqual({ totals: 'delay_impact_summary', orders: 'delay_impact' });
    expect(def.views.map((v) => v.kind)).toEqual(['kpi', 'kpi', 'vega', 'table']);
  });

  it('refuses a query reference that is not a plain [[query::id]]', () => {
    const def = parseReport({
      ...IMPACT,
      data: { a: '[[query::../../x]]', b: 'delay_impact', c: '[[query::ok_1]]' },
    })!;
    expect(def.queries).toEqual({ c: 'ok_1' });
  });

  it('says nothing for a page that is not shaped like a report', () => {
    expect(parseReport(undefined)).toBeUndefined();
    expect(parseReport({ views: 'x' })).toBeUndefined();
    expect(parseReport({ data: {}, views: [] })).toBeUndefined();
  });
});

describe('reportParams', () => {
  const def = parseReport(IMPACT)!;
  it('takes each param from the record by name, a number as a number', () => {
    expect(
      reportParams(def, {
        supplier: 'baltic-components',
        lot: 'L-24117',
        delay_days: '21',
        extra: 1,
      }),
    ).toEqual({
      supplier: 'baltic-components',
      lot: 'L-24117',
      delay_days: 21,
    });
  });
  it('shows nothing when the record lacks a param, rather than asking with a guess', () => {
    expect(reportParams(def, { supplier: 'x', lot: 'y' })).toBeUndefined();
    expect(reportParams(def, { supplier: 'x', lot: 'y', delay_days: 'soon' })).toBeUndefined();
  });
});

describe('buildReport', () => {
  const def = parseReport(IMPACT)!;
  const results = {
    totals: [{ orders_late: 4, penalty_exposure_eur: 132400, units_on_late_orders: 1160 }],
    orders: [
      { customer_order: 'SO-1', days_late: 18, status: 'late' },
      { customer_order: 'SO-2', days_late: 0, status: 'absorbed' },
    ],
  };
  const model = buildReport(def, results);

  it('turns a kpi into a labelled figure, summed over the rows, with thousands separators', () => {
    expect(model.views[0]).toEqual({ kind: 'kpi', label: 'Orders late', value: '4' });
    expect(model.views[1]).toEqual({
      kind: 'kpi',
      label: 'Penalty exposure (EUR)',
      value: '132,400',
    });
  });

  it('turns a table into plain-worded columns and string cells', () => {
    const t = model.views.find((v) => v.kind === 'table');
    expect(t).toEqual({
      kind: 'table',
      columns: ['Customer order', 'Days late', 'Status'],
      rows: [
        ['SO-1', '18', 'late'],
        ['SO-2', '0', 'absorbed'],
      ],
    });
  });

  it('notes a chart it does not draw instead of dropping it silently', () => {
    expect(model.chartsNote).toBe(true);
  });

  it('cleans untrusted cell text and caps a long table', () => {
    const many = Array.from({ length: 80 }, (_, i) => ({ id: `r${i}`, label: 'a‮b' }));
    const m = buildReport(parseReport({ ...IMPACT, views: [{ kind: 'table', data: 'orders' }] })!, {
      orders: many,
    });
    const t = m.views[0] as { rows: string[][]; more?: number };
    expect(t.rows).toHaveLength(30);
    expect(t.more).toBe(50);
    expect(t.rows[0]?.[1]).toBe('ab');
  });

  it('shows a dash for a missing value', () => {
    const m = buildReport(parseReport({ ...IMPACT, views: [{ kind: 'table', data: 'orders' }] })!, {
      orders: [{ a: null, b: 1 }],
    });
    expect((m.views[0] as { rows: string[][] }).rows[0]).toEqual(['—', '1']);
  });

  it('shows a probability as a percentage, not 0.96', () => {
    const def = parseReport({
      ...IMPACT,
      views: [
        { kind: 'kpi', data: 'totals', agg: 'max', field: 'p', label: 'Probability it lasts' },
      ],
    })!;
    expect(buildReport(def, { totals: [{ p: 0.957 }, { p: 0.4 }] }).views[0]).toEqual({
      kind: 'kpi',
      label: 'Probability it lasts',
      value: '96%',
    });
  });

  it('leaves a plain number alone even when it is a fraction, unless the label says probability', () => {
    const def = parseReport({
      ...IMPACT,
      views: [{ kind: 'kpi', data: 'totals', agg: 'sum', field: 'x', label: 'Average lead' }],
    })!;
    expect(buildReport(def, { totals: [{ x: 0.5 }] }).views[0]).toMatchObject({ value: '0.5' });
  });

  it('words a column the way a planner reads it: units in brackets, acronyms upper case', () => {
    const def = parseReport({ ...IMPACT, views: [{ kind: 'table', data: 'orders' }] })!;
    const t = buildReport(def, { orders: [{ stock_value_eur: 1, sla_target: 2, est_cost_eur: 3 }] })
      .views[0] as {
      columns: string[];
    };
    expect(t.columns).toEqual(['Stock value (EUR)', 'SLA target', 'Est cost (EUR)']);
  });

  it('hoists a column that holds the same value on every row into a line under the table', () => {
    const def = parseReport({ ...IMPACT, views: [{ kind: 'table', data: 'orders' }] })!;
    const t = buildReport(def, {
      orders: [
        { shipment_id: 'SH-1', pallets: 6, free_pallet_slots: 14, shelf_space_ok: true },
        { shipment_id: 'SH-2', pallets: 8, free_pallet_slots: 14, shelf_space_ok: true },
        { shipment_id: 'SH-3', pallets: 9, free_pallet_slots: 14, shelf_space_ok: true },
      ],
    }).views[0] as { columns: string[]; rows: string[][]; constants?: Array<[string, string]> };
    expect(t.columns).toEqual(['Shipment ID', 'Pallets']);
    expect(t.rows).toEqual([
      ['SH-1', '6'],
      ['SH-2', '8'],
      ['SH-3', '9'],
    ]);
    expect(t.constants).toEqual([
      ['Free pallet slots', '14'],
      ['Shelf space ok', 'true'],
    ]);
  });

  it('keeps every column of a one-row table, and the first column always', () => {
    const def = parseReport({ ...IMPACT, views: [{ kind: 'table', data: 'orders' }] })!;
    const one = buildReport(def, { orders: [{ a: 1, b: 2 }] }).views[0] as {
      columns: string[];
      constants?: unknown;
    };
    expect(one.columns).toEqual(['A', 'B']);
    expect(one.constants).toBeUndefined();
    const same = buildReport(def, {
      orders: [
        { a: 1, b: 2 },
        { a: 1, b: 3 },
      ],
    }).views[0] as { columns: string[] };
    expect(same.columns).toEqual(['A', 'B']);
  });

  it('writes a year as 2034, not 2,034', () => {
    const def = parseReport({ ...IMPACT, views: [{ kind: 'table', data: 'orders' }] })!;
    const t = buildReport(def, {
      orders: [{ part: 'SP-1', end_of_service_year: 2034, qty: 12345 }],
    }).views[0] as { rows: string[][] };
    expect(t.rows[0]).toEqual(['SP-1', '2034', '12,345']);
  });
});
