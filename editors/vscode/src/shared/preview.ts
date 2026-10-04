import type { ExpandResponse } from '../client/types';
import { cleanBlock, cleanText } from './untrustedText';

// The projection is whatever an upstream returned: bound it before a webview ever sees it.
const MAX_ROWS = 200;
const MAX_COLUMNS = 40;
const MAX_CELL = 500;
const MAX_FIELDS = 100;
const MAX_CHUNKS = 50;
const MAX_CHUNK = 4000;

/**
 * What a non-markdown page shows beneath its form: the data the SOURCE system holds, read-only. Built
 * from what `expand` already carries; nothing here is fetched by the extension.
 */
export type PreviewModel =
  | {
      kind: 'rows';
      readOnly: true;
      /** The view the rows were read from. */
      source: string;
      columns: string[];
      rows: string[][];
      /** More rows exist than the skill's projection limit. */
      truncated: boolean;
    }
  | { kind: 'fields'; readOnly: true; source: string; fields: { name: string; value: string }[] }
  | {
      kind: 'document';
      readOnly: true;
      chunks: { anchor: string; text: string }[];
      /** How many chunks the document has in all. */
      total: number;
      truncated: boolean;
    }
  | { kind: 'issue'; readOnly: true; source: string; code: string; message: string };

/** Any cell becomes plain text: scalars as written, structures as JSON. Never markup. */
function cell(v: unknown): string {
  if (v === null || v === undefined) return '';
  if (typeof v === 'string') return cleanText(v, MAX_CELL);
  if (typeof v === 'number' || typeof v === 'boolean') return String(v);
  return cleanText(JSON.stringify(v) ?? '', MAX_CELL);
}

const asRecord = (v: unknown): Record<string, unknown> | undefined =>
  v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : undefined;

function issueOf(raw: unknown): { code: string; message: string } {
  const rec = asRecord(raw);
  if (rec) {
    return {
      code: typeof rec.code === 'string' ? rec.code : 'source_unavailable',
      message: typeof rec.message === 'string' ? cleanText(rec.message, MAX_CELL) : cell(raw),
    };
  }
  return { code: 'source_unavailable', message: cell(raw) };
}

/** The preview for a page, or `undefined` for a plain markdown page (nothing to preview). */
export function buildPreview(e: ExpandResponse, backendKind: string): PreviewModel | undefined {
  if (backendKind === 'document') {
    const all = e.blocks ?? [];
    const chunks = all.slice(0, MAX_CHUNKS).map((b) => ({
      anchor: cleanText(String(b.anchor), 80),
      text: cleanBlock(String(b.content), MAX_CHUNK),
    }));
    const total = typeof e.chunks_total === 'number' ? e.chunks_total : all.length;
    return {
      kind: 'document',
      readOnly: true,
      chunks,
      total,
      truncated: e.chunks_truncated === true || total > chunks.length,
    };
  }
  const proj = asRecord(e.backend_projection);
  if (!proj) return undefined;
  const source =
    typeof proj.view === 'string' ? proj.view : typeof proj.source === 'string' ? proj.source : '';
  if (proj.issue !== undefined) {
    return { kind: 'issue', readOnly: true, source, ...issueOf(proj.issue) };
  }
  if (Array.isArray(proj.rows)) {
    const cut = proj.rows.length > MAX_ROWS;
    const records = proj.rows.slice(0, MAX_ROWS).map((r) => asRecord(r) ?? {});
    const columns: string[] = [];
    for (const r of records)
      for (const k of Object.keys(r))
        if (columns.length < MAX_COLUMNS && !columns.includes(k)) columns.push(k);
    return {
      kind: 'rows',
      readOnly: true,
      source,
      columns: columns.map((c) => cleanText(c, 80)),
      rows: records.map((r) => columns.map((c) => cell(r[c]))),
      truncated: proj.truncated === true || cut,
    };
  }
  const fields = asRecord(proj.fields);
  if (fields) {
    return {
      kind: 'fields',
      readOnly: true,
      source,
      fields: Object.entries(fields)
        .slice(0, MAX_FIELDS)
        .map(([name, v]) => ({ name: cleanText(name, 80), value: cell(v) })),
    };
  }
  return undefined;
}
