import { describe, expect, it } from 'vitest';
import { companionFrontmatter, isSourceField, rowSourceOf } from '../../src/shared/rowSource';

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

describe('rowSourceOf: rows from a remote (REST/MCP) upstream', () => {
  const remote = {
    kind: 'openapi',
    instances: 'rows',
    read_only: true,
    trust: 'external',
    fetched_at: '2026-10-03T12:03:44.000000Z',
    etag: 'w1:abc',
    writable_columns: ['tier'],
    rows: [{ display_name: 'Acme' }],
    source: { display_name: 'Acme', tier: 'silver' },
    linked: { enabled: true, exists: false, orphan: false },
  };

  it('knows the data is external, where it came from and what may be proposed', () => {
    expect(rowSourceOf(remote)).toEqual({
      fetchedAt: '2026-10-03T12:03:44.000000Z',
      sourceFields: ['display_name', 'tier'],
      linked: { enabled: true, exists: false, orphan: false },
      external: 'REST',
      etag: 'w1:abc',
      writableColumns: ['tier'],
    });
    expect(rowSourceOf({ ...remote, kind: 'mcp' })?.external).toBe('MCP');
  });

  it('reads a plain-string issue (the upstream could not be read) as an unavailable source', () => {
    expect(rowSourceOf({ ...remote, issue: 'upstream status 503' })?.issue).toEqual({
      code: 'source_unavailable',
      message: 'upstream status 503',
    });
  });

  it('a SQL rows projection is not external', () => {
    expect(rowSourceOf(projection)?.external).toBeUndefined();
    expect(rowSourceOf(projection)?.writableColumns).toBeUndefined();
  });
});

describe('isSourceField', () => {
  const down = {
    sourceFields: [],
    linked: { enabled: true, exists: false, orphan: false },
    issue: { code: 'source_unavailable', message: 'x' },
  };
  it('names a column the projection lists', () => {
    const row = { ...down, sourceFields: ['rating'], issue: undefined };
    expect(isSourceField(row, { name: 'rating', value: 'A', display: 'A' })).toBe(true);
    expect(isSourceField(row, { name: 'notes', value: 'n', display: 'n' })).toBe(false);
  });
  it('treats a BLANK field of a source-down row as a source column, but not one with a value', () => {
    expect(isSourceField(down, { name: 'rating', value: undefined, display: '' })).toBe(true);
    expect(isSourceField(down, { name: 'delivery_risk', value: 'low', display: 'low' })).toBe(
      false,
    );
  });
  it('is false for a page that is not a row', () => {
    expect(isSourceField(undefined, { name: 'a', value: undefined, display: '' })).toBe(false);
  });
});
