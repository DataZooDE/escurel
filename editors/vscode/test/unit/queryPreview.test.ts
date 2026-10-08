import { describe, expect, it } from 'vitest';
import {
  buildRuns,
  paramSpecs,
  queryPreviewMarkdown,
  splitValues,
} from '../../src/shared/queryPreview';

const fm = {
  title: 'Ltb quantity',
  params: [
    { name: 'part', type: 'text', required: true },
    { name: 'service_level', type: 'text', required: true },
    { name: 'qty', type: 'int' },
  ],
};

describe('query preview', () => {
  it('reads the declared parameters, ignoring junk', () => {
    expect(paramSpecs(fm)).toEqual([
      { name: 'part', type: 'text', required: true },
      { name: 'service_level', type: 'text', required: true },
      { name: 'qty', type: 'int', required: false },
    ]);
    expect(paramSpecs({ params: 'x' })).toEqual([]);
    expect(paramSpecs({ params: [{ type: 'text' }, null, { name: '' }] })).toEqual([]);
  });

  it('splits a comma list and drops empties', () => {
    expect(splitValues(' 0.95, 0.98 ,,0.99 ')).toEqual(['0.95', '0.98', '0.99']);
    expect(splitValues('')).toEqual([]);
  });

  it('one run per value of the single list parameter, numbers coerced by type', () => {
    const specs = paramSpecs(fm);
    const runs = buildRuns(specs, { part: 'SP-3307', service_level: '0.95,0.98,0.99', qty: '400' });
    expect(runs).toEqual([
      { part: 'SP-3307', service_level: '0.95', qty: 400 },
      { part: 'SP-3307', service_level: '0.98', qty: 400 },
      { part: 'SP-3307', service_level: '0.99', qty: 400 },
    ]);
  });

  it('refuses two lists, a missing required value and a non-number for a number', () => {
    const specs = paramSpecs(fm);
    expect(() => buildRuns(specs, { part: 'a,b', service_level: '1,2' })).toThrow(/one parameter/i);
    expect(() => buildRuns(specs, { part: '', service_level: '0.95' })).toThrow(/part/);
    expect(() => buildRuns(specs, { part: 'x', service_level: '1', qty: 'many' })).toThrow(/qty/);
  });

  it('caps the number of runs', () => {
    const specs = paramSpecs({ params: [{ name: 'n', type: 'int' }] });
    expect(() => buildRuns(specs, { n: '1,2,3,4,5,6,7' })).toThrow(/at most 6/);
  });

  it('renders a table per run set with the parameters, cleaned and capped', () => {
    const md = queryPreviewMarkdown({
      title: 'Ltb quantity',
      id: 'ltb_quantity',
      description: 'Quantity that holds a service level.',
      runs: [
        {
          params: { part: 'SP-3307', service_level: '0.95' },
          rows: [{ ltb_qty: 634, stock_value_eur: 748120, note: 'a‮b|c' }],
        },
        {
          params: { part: 'SP-3307', service_level: '0.98' },
          rows: [{ ltb_qty: 666, stock_value_eur: 785880, note: '' }],
        },
        { params: { part: 'x', service_level: '2' }, rows: [], error: 'bad level' },
      ],
    });
    expect(md).toContain('# Ltb quantity');
    expect(md).toContain('| service level | ltb qty | stock value eur | note |');
    expect(md).toContain('| 0.95 | 634 | 748,120 |');
    expect(md).toContain('| 0.98 | 666 | 785,880 |');
    expect(md).not.toContain('‮');
    expect(md).not.toMatch(/a.b\|c/); // the pipe cannot break the table
    expect(md).toContain('bad level');
    expect(md).toContain('nothing is saved');
  });

  it('says so when a run returns nothing', () => {
    const md = queryPreviewMarkdown({
      title: 't',
      id: 'x',
      description: '',
      runs: [{ params: { a: '1' }, rows: [] }],
    });
    expect(md).toContain('no rows');
  });
  it('says what is the same on every row once, and puts the numbers first', () => {
    const rows = (q: number, v: number) => ({
      service_level: 'repeats the parameter',
      part_name: 'Servo drive module',
      eos_date: '2034-12-31',
      stock_value_eur: v,
      ltb_qty: q,
    });
    const md = queryPreviewMarkdown({
      title: 't',
      id: 'x',
      description: '',
      runs: [
        { params: { service_level: '0.95' }, rows: [rows(634, 748120)] },
        { params: { service_level: '0.98' }, rows: [rows(666, 785880)] },
      ],
    });
    expect(md).toContain('**part name** Servo drive module');
    expect(md).toContain('**eos date** 2034-12-31');
    const header = md.split('\n').find((l) => l.startsWith('| service level'))!;
    expect(header).toBe('| service level | stock value eur | ltb qty |');
    expect(md).not.toContain('repeats the parameter');
  });
});
