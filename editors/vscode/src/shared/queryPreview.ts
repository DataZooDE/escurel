import { cleanText } from './untrustedText';

/** "Preview with parameters": run a query page with values the person types, show the rows. */

export interface ParamSpec {
  name: string;
  type: string;
  required: boolean;
}

export interface PreviewRun {
  params: Record<string, unknown>;
  rows: Array<Record<string, unknown>>;
  error?: string;
}

const MAX_RUNS = 6;
const MAX_ROWS = 30;

/** The parameters a query page declares (`params: [{name, type, required}]`); anything else is ignored. */
export function paramSpecs(frontmatter: Record<string, unknown> | undefined): ParamSpec[] {
  const raw = frontmatter?.params;
  if (!Array.isArray(raw)) return [];
  const out: ParamSpec[] = [];
  for (const p of raw) {
    if (!p || typeof p !== 'object') continue;
    const { name, type, required } = p as Record<string, unknown>;
    if (typeof name !== 'string' || !name) continue;
    out.push({
      name,
      type: typeof type === 'string' ? type : 'text',
      required: required === true,
    });
  }
  return out;
}

/** `0.95, 0.98` -> ['0.95', '0.98']. */
export function splitValues(raw: string): string[] {
  return raw
    .split(',')
    .map((v) => v.trim())
    .filter((v) => v !== '');
}

const NUMERIC = new Set(['int', 'integer', 'number', 'float', 'double', 'decimal', 'bigint']);

function coerce(spec: ParamSpec, value: string): unknown {
  if (!NUMERIC.has(spec.type)) return value;
  const n = Number(value);
  if (!Number.isFinite(n)) throw new Error(`${spec.name} must be a number, not "${value}"`);
  return n;
}

/**
 * One parameter set per run. At most ONE parameter may hold a comma list (each value is a run: the
 * presenter compares 0.95, 0.98 and 0.99 side by side); the others are single values.
 */
export function buildRuns(
  specs: ParamSpec[],
  answers: Record<string, string | undefined>,
): Array<Record<string, unknown>> {
  const values = new Map<string, string[]>();
  for (const s of specs) {
    const vs = splitValues(answers[s.name] ?? '');
    if (vs.length === 0) {
      if (s.required) throw new Error(`${s.name} is required`);
      continue;
    }
    values.set(s.name, vs);
  }
  const lists = [...values].filter(([, vs]) => vs.length > 1);
  if (lists.length > 1) throw new Error('Only one parameter may hold a list of values');
  const [listName, listValues] = lists[0] ?? [undefined, ['']];
  if (listValues.length > MAX_RUNS) throw new Error(`Compare at most ${MAX_RUNS} values at once`);
  return listValues.map((lv) => {
    const params: Record<string, unknown> = {};
    for (const s of specs) {
      const vs = values.get(s.name);
      if (!vs) continue;
      params[s.name] = coerce(s, s.name === listName ? lv : vs[0]!);
    }
    return params;
  });
}

const label = (k: string): string => k.replaceAll('_', ' ');

function cell(v: unknown): string {
  if (v === null || v === undefined) return '';
  if (typeof v === 'number')
    return Number.isInteger(v) && Math.abs(v) >= 10000 ? v.toLocaleString('en-US') : String(v);
  if (typeof v === 'object') return cleanText(JSON.stringify(v), 120).replaceAll('|', '/');
  return cleanText(String(v), 120).replaceAll('|', '/');
}

/** The preview as markdown (shown in VS Code's own preview): one table, the compared parameter first. */
export function queryPreviewMarkdown(input: {
  title: string;
  id: string;
  description: string;
  runs: PreviewRun[];
}): string {
  const { runs } = input;
  const paramNames = [...new Set(runs.flatMap((r) => Object.keys(r.params)))];
  const varying = paramNames.filter(
    (n) => new Set(runs.map((r) => String(r.params[n] ?? ''))).size > 1,
  );
  const fixed = paramNames.filter((n) => !varying.includes(n));
  const lines: string[] = [`# ${cleanText(input.title, 120)}`, ''];
  if (input.description) lines.push(cleanText(input.description, 400), '');
  if (fixed.length)
    lines.push(fixed.map((n) => `**${label(n)}** ${cell(runs[0]?.params[n])}`).join(' · '), '');
  const first = runs.find((r) => r.rows.length > 0)?.rows[0];
  const all = runs.flatMap((r) => r.rows);
  // A column that only repeats a parameter (part, service_level) is already said by the parameters.
  const named = (first ? Object.keys(first) : []).filter((c) => !paramNames.includes(c));
  // What is the same on every row is said once, above the table; the numbers lead the table.
  const constant =
    all.length >= 2
      ? named.filter((c) => new Set(all.map((row) => JSON.stringify(row[c] ?? null))).size === 1)
      : [];
  const isNumber = (c: string) => typeof first?.[c] === 'number';
  const rest = named.filter((c) => !constant.includes(c));
  const cols = [...rest.filter(isNumber), ...rest.filter((c) => !isNumber(c))];
  if (constant.length)
    lines.push(constant.map((c) => `**${label(c)}** ${cell(first![c])}`).join(' · '), '');
  const head = [...varying, ...cols].map(label);
  lines.push(`| ${head.join(' | ')} |`, `|${head.map(() => '---').join('|')}|`);
  let any = false;
  for (const r of runs) {
    const lead = varying.map((n) => cell(r.params[n]));
    if (r.error) {
      lines.push(
        `| ${[...lead, `**${cleanText(r.error, 200).replaceAll('|', '/')}**`, ...cols.slice(1).map(() => '')].join(' | ')} |`,
      );
      any = true;
      continue;
    }
    for (const row of r.rows.slice(0, MAX_ROWS)) {
      any = true;
      lines.push(`| ${[...lead, ...cols.map((c) => cell(row[c]))].join(' | ')} |`);
    }
  }
  if (!any) lines.push(`| ${head.map(() => '').join(' | ')} |`);
  if (runs.every((r) => r.rows.length === 0 && !r.error))
    lines.push('', '_The query returned no rows._');
  lines.push(
    '',
    `_A preview of page \`${cleanText(input.id, 80)}\`: the query runs against the live data and nothing is saved. Change the page, and the agents that use it follow the reviewed version._`,
  );
  return lines.join('\n');
}
