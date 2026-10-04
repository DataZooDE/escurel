import { describe, expect, it } from 'vitest';
import type { ExpandResponse } from '../../src/client/types';
import { buildPreview } from '../../src/shared/preview';

const page = {
  page_id: 'markdown/instances/order-lines__all.md',
  slug: 'all',
  skill: 'order-lines',
  page_kind: 'instance',
};
const expand = (extra: Partial<ExpandResponse> & Record<string, unknown>): ExpandResponse =>
  ({ page, frontmatter: {}, body: '', blocks: [], wikilinks_out: [], ...extra }) as ExpandResponse;

// The shapes below are what `expand` returns on the wire for each backend (crates/escurel-server
// src/mcp/tools_read.rs: sql_view_projection, fetch_projection and the document branch).
describe('buildPreview', () => {
  it('a plain markdown page has no preview', () => {
    expect(buildPreview(expand({}), 'markdown')).toBeUndefined();
  });

  it('a sql_view page previews its rows as a read-only table, columns in first-seen order', () => {
    const p = buildPreview(
      expand({
        backend_projection: {
          view: 'vw_order_lines_all',
          rows: [
            { vbeln: 4500123, posnr: 10, matnr: 'GH-4711', netwr: 62400.0 },
            { vbeln: 4500131, posnr: 20, matnr: 'TH-0815', netwr: null },
          ],
          source: {},
          truncated: true,
        },
      }),
      'sql_view',
    );
    expect(p).toEqual({
      kind: 'rows',
      readOnly: true,
      source: 'vw_order_lines_all',
      columns: ['vbeln', 'posnr', 'matnr', 'netwr'],
      rows: [
        ['4500123', '10', 'GH-4711', '62400'],
        ['4500131', '20', 'TH-0815', ''],
      ],
      truncated: true,
    });
  });

  it('an empty projection still names its source and says there are no rows', () => {
    const p = buildPreview(
      expand({ backend_projection: { view: 'v', rows: [], source: {}, truncated: false } }),
      'sql_view',
    );
    expect(p).toMatchObject({ kind: 'rows', columns: [], rows: [], truncated: false });
  });

  it('a degraded or unavailable source is shown as the issue, not as rows', () => {
    const p = buildPreview(
      expand({
        backend_projection: {
          view: 'v',
          rows: [],
          source: {},
          issue: { code: 'binding_degraded', message: 'source schema drifted' },
        },
      }),
      'sql_view',
    );
    expect(p).toEqual({
      kind: 'issue',
      readOnly: true,
      source: 'v',
      code: 'binding_degraded',
      message: 'source schema drifted',
    });
  });

  it('an openapi or mcp page previews the live fields it fetched, each as name and text', () => {
    const p = buildPreview(
      expand({
        backend_projection: {
          source: 'sap-api',
          fields: { status: 'open', amount: 12.5, tags: ['a', 'b'] },
        },
      }),
      'openapi',
    );
    expect(p).toEqual({
      kind: 'fields',
      readOnly: true,
      source: 'sap-api',
      fields: [
        { name: 'status', value: 'open' },
        { name: 'amount', value: '12.5' },
        { name: 'tags', value: '["a","b"]' },
      ],
    });
  });

  it('a remote page whose upstream failed shows the issue', () => {
    const p = buildPreview(
      expand({ backend_projection: { issue: 'endpoint `x` is not registered' } }),
      'mcp',
    );
    expect(p).toMatchObject({ kind: 'issue', message: 'endpoint `x` is not registered' });
  });

  it('a document page previews its lead chunks and says how many there are in all', () => {
    const p = buildPreview(
      expand({
        blocks: [
          { anchor: 'c1', content: 'First chunk of text.' },
          { anchor: 'c2', content: 'Second chunk.' },
        ],
        chunks_total: 40,
        chunks_truncated: true,
      }),
      'document',
    );
    expect(p).toEqual({
      kind: 'document',
      readOnly: true,
      chunks: [
        { anchor: 'c1', text: 'First chunk of text.' },
        { anchor: 'c2', text: 'Second chunk.' },
      ],
      total: 40,
      truncated: true,
    });
  });

  it('never trusts a value: objects and arrays are rendered as JSON text, never as markup', () => {
    const p = buildPreview(
      expand({ backend_projection: { view: 'v', rows: [{ a: { x: '<script>' } }], source: {} } }),
      'sql_view',
    );
    expect(p).toMatchObject({ rows: [['{"x":"<script>"}']] });
  });
});

// The projection is whatever an upstream returned. The host bounds it before it reaches a webview: a
// hostile or buggy source must not be able to send a million cells or a page-long cell, and what it sends
// is text (no bidi overrides, no control characters).
describe('buildPreview bounds untrusted data', () => {
  it('caps rows, columns and the length of a cell, and says the rows were cut', () => {
    const rows = Array.from({ length: 600 }, (_, i) => ({ id: i, big: 'x'.repeat(10_000) }));
    const wide = Object.fromEntries(Array.from({ length: 100 }, (_, i) => [`c${i}`, i]));
    const m = buildPreview(
      expand({ backend_projection: { view: 'v', rows: [...rows, wide] } }),
      'sql_view',
    );
    if (m?.kind !== 'rows') throw new Error('rows expected');
    expect(m.rows.length).toBeLessThanOrEqual(200);
    expect(m.truncated).toBe(true);
    expect(m.columns.length).toBeLessThanOrEqual(40);
    for (const r of m.rows) for (const c of r) expect(c.length).toBeLessThanOrEqual(500);
  });

  it('strips bidi overrides and control characters from every cell, field and issue', () => {
    const evil = 'inv\u202Eexe.pdf\u0007';
    const r = buildPreview(
      expand({ backend_projection: { view: 'v', rows: [{ a: evil }] } }),
      'sql_view',
    );
    if (r?.kind !== 'rows') throw new Error('rows expected');
    expect(r.rows[0]![0]).toBe('invexe.pdf');
    const f = buildPreview(
      expand({ backend_projection: { source: 's', fields: { name: evil } } }),
      'openapi',
    );
    if (f?.kind !== 'fields') throw new Error('fields expected');
    expect(f.fields[0]!.value).toBe('invexe.pdf');
    const i = buildPreview(
      expand({ backend_projection: { view: 'v', issue: { code: 'x', message: evil } } }),
      'sql_view',
    );
    if (i?.kind !== 'issue') throw new Error('issue expected');
    expect(i.message).toBe('invexe.pdf');
  });

  it('keeps line breaks in a document chunk but bounds its length and strips controls', () => {
    const m = buildPreview(
      expand({
        blocks: [{ anchor: 'a', content: `line one\nline two\u202E${'y'.repeat(9000)}` }] as never,
      }),
      'document',
    );
    if (m?.kind !== 'document') throw new Error('document expected');
    expect(m.chunks[0]!.text.startsWith('line one\nline two')).toBe(true);
    expect(m.chunks[0]!.text).not.toContain('\u202E');
    expect(m.chunks[0]!.text.length).toBeLessThanOrEqual(4000);
  });
});
