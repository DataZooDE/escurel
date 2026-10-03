import { describe, expect, it } from 'vitest';
import { companionFrontmatter, rowSourceOf } from '../../src/shared/rowSource';

// An `instances: rows` skill's page is ONE row of a read-only source plus an optional linked markdown.
// `expand` merges them for reading; the extension must show the row as the read-only thing it is and
// let a person edit only the notes.
const projection = {
  view: 'vw_customer_order__rows',
  instances: 'rows',
  read_only: true,
  fetched_at: '2026-10-03T12:03:44.000000Z',
  rows: [{ vbeln: 4500131 }],
  source: { sales_doc: 4500131, sold_to_name: 'Kessler Werkzeugbau GmbH' },
  truncated: false,
  linked: { enabled: true, exists: false, orphan: false },
  columns: [{ name: 'vbeln', type: 'BIGINT', kind: 'int' }],
};

describe('rowSourceOf', () => {
  it('reads the row facts a person needs: read-only, how fresh, whether notes exist', () => {
    expect(rowSourceOf(projection)).toEqual({
      fetchedAt: '2026-10-03T12:03:44.000000Z',
      sourceFields: ['sales_doc', 'sold_to_name'],
      linked: { enabled: true, exists: false, orphan: false },
    });
  });

  it('carries the issue when the source row is gone or the source is unavailable', () => {
    const gone = {
      ...projection,
      rows: [],
      source: {},
      linked: { enabled: true, exists: true, orphan: true },
      issue: { code: 'source_missing', message: 'the source has no such row; the notes are kept' },
    };
    expect(rowSourceOf(gone)?.issue).toEqual({
      code: 'source_missing',
      message: 'the source has no such row; the notes are kept',
    });
    expect(rowSourceOf(gone)?.linked.orphan).toBe(true);
  });

  it('is undefined for anything that is not a rows projection', () => {
    expect(rowSourceOf(undefined)).toBeUndefined();
    expect(rowSourceOf({ view: 'vw_x', rows: [] })).toBeUndefined();
    expect(rowSourceOf({ instances: 'view' })).toBeUndefined();
    expect(rowSourceOf('nope')).toBeUndefined();
  });
});

describe('companionFrontmatter', () => {
  it('drops the row’s source columns: only the notes’ own fields are editable', () => {
    const merged = {
      kind: 'instance',
      id: 'order-4500131',
      skill: 'customer-order',
      delivery_risk: 'low',
      sales_doc: 4500131,
      sold_to_name: 'Kessler Werkzeugbau GmbH',
    };
    expect(companionFrontmatter(merged, projection)).toEqual({
      kind: 'instance',
      id: 'order-4500131',
      skill: 'customer-order',
      delivery_risk: 'low',
    });
  });

  it('leaves an ordinary page alone', () => {
    const fm = { kind: 'instance', id: 'a', skill: 's', x: 1 };
    expect(companionFrontmatter(fm, undefined)).toEqual(fm);
  });
});
