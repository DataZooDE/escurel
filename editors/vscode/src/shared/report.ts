// A report skill (Peacock) says what to draw from a skill's records: params, named data (authored
// queries) and views. The workbench draws what it can with its own widgets, KPI figures and tables,
// and says so when a chart is left out. The report page is the SKILL AUTHOR's text: every cell and
// label goes through the shared cleaners, and a query id must be a plain id.
import { cleanText } from './untrustedText';

export interface ReportDef {
  title: string;
  params: Array<{ name: string; type: string }>;
  /** data name -> query id (`[[query::id]]`). */
  queries: Record<string, string>;
  views: ReportViewDef[];
}
export interface ReportViewDef {
  kind: string;
  data?: string;
  agg?: string;
  field?: string;
  label?: string;
}

export type ReportView =
  | { kind: 'kpi'; label: string; value: string }
  | { kind: 'table'; columns: string[]; rows: string[][]; more?: number };
export interface ReportModel {
  title: string;
  views: ReportView[];
  /** The report asks for a chart this view does not draw. */
  chartsNote?: true;
}

type Row = Record<string, unknown>;
const QUERY_REF = /^\[\[query::([A-Za-z0-9][A-Za-z0-9_.-]*)\]\]$/;
const MAX_ROWS = 30;
const MAX_COLUMNS = 12;

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/** The report a skill page's frontmatter describes, or undefined when it is not shaped like one. */
export function parseReport(fm: unknown): ReportDef | undefined {
  if (!isObject(fm) || !isObject(fm.data) || !Array.isArray(fm.views)) return undefined;
  const queries: Record<string, string> = {};
  for (const [name, ref] of Object.entries(fm.data)) {
    const m = typeof ref === 'string' ? QUERY_REF.exec(ref) : null;
    if (m?.[1]) queries[name] = m[1];
  }
  if (Object.keys(queries).length === 0) return undefined;
  const params = isObject(fm.params)
    ? Object.entries(fm.params).map(([name, p]) => ({
        name,
        type: isObject(p) && typeof p.type === 'string' ? p.type : 'string',
      }))
    : [];
  const views: ReportViewDef[] = fm.views.filter(isObject).map((v) => ({
    kind: typeof v.kind === 'string' ? v.kind : '',
    ...(typeof v.data === 'string' ? { data: v.data } : {}),
    ...(typeof v.agg === 'string' ? { agg: v.agg } : {}),
    ...(typeof v.field === 'string' ? { field: v.field } : {}),
    ...(typeof v.label === 'string' ? { label: v.label } : {}),
  }));
  const title =
    typeof fm.title === 'string' && fm.title.trim() ? fm.title : String(fm.id ?? 'Report');
  return { title: cleanText(title, 80), params, queries, views };
}

/** The report's params read from the record by name; undefined when one is missing or not a number where one is needed. */
export function reportParams(
  def: ReportDef,
  record: Record<string, unknown>,
): Record<string, string | number> | undefined {
  const out: Record<string, string | number> = {};
  for (const p of def.params) {
    const v = record[p.name];
    if (v === undefined || v === null || v === '') return undefined;
    if (p.type === 'number' || p.type === 'int' || p.type === 'integer') {
      const n = typeof v === 'number' ? v : Number(v);
      if (!Number.isFinite(n)) return undefined;
      out[p.name] = n;
    } else out[p.name] = String(v);
  }
  return out;
}

function humanise(key: string): string {
  const s = key.replace(/[_-]+/g, ' ').trim();
  return cleanText(s.charAt(0).toUpperCase() + s.slice(1), 40);
}

function format(v: unknown): string {
  if (v === null || v === undefined || v === '') return '—';
  if (typeof v === 'number' || typeof v === 'bigint') {
    const n = Number(v);
    return Number.isInteger(n)
      ? n.toLocaleString('en-US')
      : n.toLocaleString('en-US', { maximumFractionDigits: 2 });
  }
  return cleanText(String(v), 120);
}

function aggregate(rows: Row[], field: string, agg: string): number | undefined {
  const values = rows.map((r) => Number(r[field])).filter((n) => Number.isFinite(n));
  if (agg === 'count') return rows.length;
  if (values.length === 0) return undefined;
  switch (agg) {
    case 'max':
      return Math.max(...values);
    case 'min':
      return Math.min(...values);
    case 'avg':
      return values.reduce((a, b) => a + b, 0) / values.length;
    default:
      return values.reduce((a, b) => a + b, 0);
  }
}

export function buildReport(def: ReportDef, results: Record<string, Row[]>): ReportModel {
  const views: ReportView[] = [];
  let chartsNote = false;
  for (const v of def.views) {
    const rows = v.data ? (results[v.data] ?? []) : [];
    if (v.kind === 'kpi' && v.field) {
      const n = aggregate(rows, v.field, v.agg ?? 'sum');
      const label = cleanText(v.label ?? humanise(v.field), 60);
      // A probability is read as a percentage: 0.957 is "96%".
      const value =
        n !== undefined && n >= 0 && n <= 1 && /probab|percent|share/i.test(label)
          ? `${Math.round(n * 100)}%`
          : format(n);
      views.push({ kind: 'kpi', label, value });
    } else if (v.kind === 'table') {
      if (rows.length === 0) continue;
      const keys = Object.keys(rows[0] ?? {}).slice(0, MAX_COLUMNS);
      const shown = rows.slice(0, MAX_ROWS);
      views.push({
        kind: 'table',
        columns: keys.map(humanise),
        rows: shown.map((r) => keys.map((k) => format(r[k]))),
        ...(rows.length > shown.length ? { more: rows.length - shown.length } : {}),
      });
    } else if (v.kind === 'vega') chartsNote = true;
  }
  return { title: def.title, views, ...(chartsNote ? { chartsNote: true as const } : {}) };
}
