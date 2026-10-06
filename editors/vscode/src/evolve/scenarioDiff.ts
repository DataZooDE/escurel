import type { TokenRefresher } from '../auth/refresher';
import { evolveOrigin } from './holdoutClient';

/** One table's exact change counts, as Evolve's comparison record reports them. */
export interface ScenarioTableDiff {
  table: string;
  rowsAdded: number;
  rowsRemoved: number;
  rowsModified: number;
}

/** A row-level change. Key columns sit beside the fixed fields, exactly as `scenario_diff` emits them. */
export interface ScenarioDiffRow {
  table: string;
  changeType: 'added' | 'removed' | 'modified';
  columnName: string | null;
  oldValue: string | null;
  newValue: string | null;
  key: Record<string, unknown>;
}

/** An immutable comparison result read from Evolve. */
export interface Comparison {
  comparison: string;
  experiment: string;
  state: 'completed' | 'blocked';
  reason: string | null;
  baselineProgramId: number;
  candidateProgramId: number;
  tables: ScenarioTableDiff[];
  rows: ScenarioDiffRow[];
  truncated: boolean;
  evidenceNote: string;
  resultSha256: string;
  /** Set while Evolve has more stored rows to return for this comparison. */
  nextCursor?: string;
}

const FIXED = new Set(['table', 'change_type', 'column_name', 'old_value', 'new_value']);
/** Pages of stored rows read for one comparison; the stored result is already capped. */
const MAX_PAGES = 10;

function bad(): never {
  throw new Error('Evolve did not return a comparison. Check the Evolve endpoint and version.');
}

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) bad();
  return value as Record<string, unknown>;
}

function text(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

function parseRow(entry: unknown): ScenarioDiffRow {
  const r = record(entry);
  const changeType = r.change_type;
  if (
    typeof r.table !== 'string' ||
    !['added', 'removed', 'modified'].includes(changeType as string)
  )
    bad();
  const key: Record<string, unknown> = {};
  for (const [name, v] of Object.entries(r)) if (!FIXED.has(name)) key[name] = v;
  return {
    table: r.table,
    changeType: changeType as ScenarioDiffRow['changeType'],
    columnName: text(r.column_name),
    oldValue: text(r.old_value),
    newValue: text(r.new_value),
    key,
  };
}

export function parseComparison(value: unknown): Comparison {
  const body = record(value);
  if (
    typeof body.comparison !== 'string' ||
    typeof body.experiment !== 'string' ||
    (body.state !== 'completed' && body.state !== 'blocked') ||
    !Array.isArray(body.tables) ||
    !Array.isArray(body.rows)
  )
    bad();
  const tables = (body.tables as unknown[]).map((entry) => {
    const t = record(entry);
    if (typeof t.table !== 'string') bad();
    const count = (k: string): number => (typeof t[k] === 'number' ? (t[k] as number) : 0);
    return {
      table: t.table as string,
      rowsAdded: count('rows_added'),
      rowsRemoved: count('rows_removed'),
      rowsModified: count('rows_modified'),
    };
  });
  const id = (k: string): number => (typeof body[k] === 'number' ? (body[k] as number) : 0);
  return {
    comparison: body.comparison,
    experiment: body.experiment,
    state: body.state,
    reason: text(body.reason),
    baselineProgramId: id('baseline_program_id'),
    candidateProgramId: id('candidate_program_id'),
    tables,
    rows: (body.rows as unknown[]).map(parseRow),
    truncated: body.truncated === true,
    evidenceNote: typeof body.evidence_note === 'string' ? body.evidence_note : '',
    resultSha256: typeof body.result_sha256 === 'string' ? body.result_sha256 : '',
    ...(typeof body.next_cursor === 'string' ? { nextCursor: body.next_cursor } : {}),
  };
}

/**
 * A comparison page is a request plus a readable summary; Evolve's record is the result. The
 * page may only be trusted to describe a result when it carries that record's hash, so a page
 * that merely claims to be completed (an owner can write any frontmatter when creating one)
 * never vouches for anything.
 */
export function matchesPage(comparison: Comparison, pageResultSha256: string | undefined): boolean {
  return (
    comparison.state === 'completed' &&
    !!pageResultSha256 &&
    comparison.resultSha256 === pageResultSha256
  );
}

function keyLabel(key: Record<string, unknown>): string {
  const parts = Object.entries(key).map(([name, v]) => `${name}=${String(v)}`);
  return parts.join(', ') || '(row)';
}

/**
 * Two virtual documents for the native diff editor. A line is one changed cell (or one whole
 * row that exists on one side only), so the editor highlights exactly what changed.
 */
export function comparisonTexts(
  comparison: Comparison,
  table: string,
): { baseline: string; candidate: string } {
  const baseline: string[] = [];
  const candidate: string[] = [];
  for (const row of comparison.rows) {
    if (row.table !== table) continue;
    const key = keyLabel(row.key);
    if (row.changeType === 'modified') {
      baseline.push(`${key} · ${row.columnName} = ${row.oldValue}`);
      candidate.push(`${key} · ${row.columnName} = ${row.newValue}`);
    } else if (row.changeType === 'removed') {
      baseline.push(`${key} · (row)`);
    } else {
      candidate.push(`${key} · (row)`);
    }
  }
  const note = comparison.truncated
    ? ['', '# Rows were truncated: the counts are exact, this list is not complete.']
    : [];
  const header = (side: string, program: number): string =>
    `# ${table} · ${side} program ${program}`;
  return {
    baseline:
      [header('baseline', comparison.baselineProgramId), ...baseline, ...note].join('\n') + '\n',
    candidate:
      [header('candidate', comparison.candidateProgramId), ...candidate, ...note].join('\n') + '\n',
  };
}

/** Plain-language lines for a tooltip or status message; always says what the comparison is not. */
export function comparisonSummary(comparison: Comparison): string[] {
  const lines = comparison.tables.map(
    (t) => `${t.table}: ${t.rowsModified} modified, ${t.rowsAdded} added, ${t.rowsRemoved} removed`,
  );
  if (!lines.length) lines.push('The candidate wrote exactly the same state as the baseline.');
  lines.push(
    'Search-time replay on the training instance: an explanation of what changed, not independent validation.',
  );
  return lines;
}

/** Evolve has no record of the comparison (or it is another owner's: the answer is the same). */
export class ComparisonNotFoundError extends Error {
  constructor() {
    super('Evolve does not know this comparison (not found).');
    this.name = 'ComparisonNotFoundError';
  }
}

function rejectionMessage(status: number): string {
  if (status === 401)
    return 'Evolve rejected the signed-in token. Check its OIDC audience and sign in again.';
  if (status === 404) return 'Evolve does not know this comparison (not found).';
  return `Evolve could not return the comparison (HTTP ${status}).`;
}

async function fetchPage(
  origin: string,
  refresher: TokenRefresher,
  body: Record<string, unknown>,
): Promise<unknown> {
  let lastStatus = 0;
  for (let attempt = 0; attempt < 2; attempt++) {
    const token =
      attempt === 1 && lastStatus === 401 ? await refresher.invalidate() : await refresher.get();
    if (!token)
      throw new Error('Sign in with an OIDC token accepted by Evolve to compare scenarios.');
    let response: Response;
    try {
      response = await fetch(origin + '/', {
        method: 'POST',
        redirect: 'error',
        signal: AbortSignal.timeout(30_000),
        headers: {
          authorization: `Bearer ${token}`,
          'content-type': 'application/json',
          'X-Triton-Tool': 'evolve_comparison',
        },
        body: JSON.stringify(body),
      });
    } catch {
      throw new Error('Evolve did not answer. Check that the service is running and reachable.');
    }
    lastStatus = response.status;
    if (response.status === 401 && attempt === 0) continue;
    if (response.status === 404) throw new ComparisonNotFoundError();
    if (!response.ok) throw new Error(rejectionMessage(response.status));
    return response.json();
  }
  throw new Error(rejectionMessage(lastStatus));
}

/** Read one comparison from Evolve, following the cursor through every stored row. */
export async function fetchComparison(
  endpoint: string,
  refresher: TokenRefresher,
  comparisonId: string,
): Promise<Comparison> {
  const origin = evolveOrigin(endpoint);
  let result = parseComparison(await fetchPage(origin, refresher, { comparison: comparisonId }));
  let next = result.nextCursor;
  for (let page = 1; next !== undefined && page < MAX_PAGES; page++) {
    const more = parseComparison(
      await fetchPage(origin, refresher, { comparison: comparisonId, cursor: next }),
    );
    result = { ...result, rows: [...result.rows, ...more.rows] };
    next = more.nextCursor;
  }
  const { nextCursor, ...complete } = result;
  void nextCursor;
  return complete;
}
