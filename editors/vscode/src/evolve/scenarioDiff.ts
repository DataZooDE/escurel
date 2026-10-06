import type { TokenRefresher } from '../auth/refresher';
import { evolveOrigin } from './holdoutClient';

/** One table's exact change counts, as Evolve's `evolve_scenario_diff` reports them. */
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

export interface ScenarioDiff {
  experiment: string;
  pilot: string;
  baselineProgramId: number;
  winnerProgramId: number;
  tables: ScenarioTableDiff[];
  rows: ScenarioDiffRow[];
  truncated: boolean;
  evidenceNote: string;
}

const FIXED = new Set(['table', 'change_type', 'column_name', 'old_value', 'new_value']);

function bad(): never {
  throw new Error('Evolve did not return a scenario diff. Check the Evolve endpoint and version.');
}

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) bad();
  return value as Record<string, unknown>;
}

function text(value: unknown): string | null {
  return typeof value === 'string' ? value : null;
}

export function parseScenarioDiff(value: unknown): ScenarioDiff {
  const body = record(value);
  if (
    typeof body.experiment !== 'string' ||
    !Array.isArray(body.tables) ||
    !Array.isArray(body.rows) ||
    typeof body.winner_program_id !== 'number'
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
  const rows = (body.rows as unknown[]).map((entry): ScenarioDiffRow => {
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
  });
  return {
    experiment: body.experiment,
    pilot: typeof body.pilot === 'string' ? body.pilot : '',
    baselineProgramId: typeof body.baseline_program_id === 'number' ? body.baseline_program_id : 0,
    winnerProgramId: body.winner_program_id as number,
    tables,
    rows,
    truncated: body.truncated === true,
    evidenceNote: typeof body.evidence_note === 'string' ? body.evidence_note : '',
  };
}

function keyLabel(key: Record<string, unknown>): string {
  const parts = Object.entries(key).map(([name, v]) => `${name}=${String(v)}`);
  return parts.join(', ') || '(row)';
}

/**
 * Two virtual documents for the native diff editor. A line is one changed cell (or one whole
 * row that exists on one side only), so the editor highlights exactly what the winner changed.
 * Output is sorted by the order Evolve returned, which is deterministic.
 */
export function scenarioDiffTexts(
  diff: ScenarioDiff,
  table: string,
): { seed: string; winner: string } {
  const seed: string[] = [];
  const winner: string[] = [];
  for (const row of diff.rows) {
    if (row.table !== table) continue;
    const key = keyLabel(row.key);
    if (row.changeType === 'modified') {
      seed.push(`${key} · ${row.columnName} = ${row.oldValue}`);
      winner.push(`${key} · ${row.columnName} = ${row.newValue}`);
    } else if (row.changeType === 'removed') {
      seed.push(`${key} · (row)`);
    } else {
      winner.push(`${key} · (row)`);
    }
  }
  const header = `# ${table} · seed program ${diff.baselineProgramId} vs winner ${diff.winnerProgramId}`;
  const note = diff.truncated
    ? ['', '# Rows were truncated: the counts are exact, this list is not complete.']
    : [];
  return {
    seed: [header, ...seed, ...note].join('\n') + '\n',
    winner: [header, ...winner, ...note].join('\n') + '\n',
  };
}

/** Plain-language lines for a summary panel or tooltip; always says what the diff is not. */
export function scenarioDiffSummary(diff: ScenarioDiff): string[] {
  const lines = diff.tables.map(
    (t) => `${t.table}: ${t.rowsModified} modified, ${t.rowsAdded} added, ${t.rowsRemoved} removed`,
  );
  if (!lines.length) lines.push('The winner wrote exactly the same state as the seed.');
  lines.push(
    'Search-time replay on the training instance: an explanation of what changed, not independent validation.',
  );
  return lines;
}

function rejectionMessage(status: number): string {
  if (status === 401)
    return 'Evolve rejected the signed-in token. Check its OIDC audience and sign in again.';
  if (status === 403) return 'This experiment belongs to another owner.';
  if (status === 404) return 'Evolve does not know this experiment.';
  if (status === 422)
    return 'Scenario diffs are not available for this experiment yet (this pilot does not support them).';
  return `Evolve could not produce the scenario diff (HTTP ${status}).`;
}

export async function fetchScenarioDiff(
  endpoint: string,
  refresher: TokenRefresher,
  experiment: string,
): Promise<ScenarioDiff> {
  const origin = evolveOrigin(endpoint);
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
          'X-Triton-Tool': 'evolve_scenario_diff',
        },
        body: JSON.stringify({ experiment }),
      });
    } catch {
      throw new Error('Evolve did not answer. Check that the service is running and reachable.');
    }
    lastStatus = response.status;
    if (response.status === 401 && attempt === 0) continue;
    if (!response.ok) throw new Error(rejectionMessage(response.status));
    return parseScenarioDiff(await response.json());
  }
  throw new Error(rejectionMessage(lastStatus));
}
