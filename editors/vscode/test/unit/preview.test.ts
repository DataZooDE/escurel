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
