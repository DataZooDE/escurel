import type { Instance } from '../client/types';
import type { Comparison } from '../evolve/scenarioDiff';

/** Virtual documents for the baseline-vs-candidate diff are served under this scheme. */
export const SCENARIO_SCHEME = 'escurel-scenario';

export type ComparisonStatus = 'requested' | 'completed' | 'blocked' | 'unknown';

export interface ComparisonPageRow {
  kind: 'comparison';
  comparison: string;
  label: string;
  description: string;
  status: ComparisonStatus;
  experiment: string;
  /** What the page says about a result; only Evolve's record decides whether it is true. */
  pageResultSha256?: string;
  reason?: string;
}

export interface TableRow {
  kind: 'table';
  table: string;
  label: string;
  description: string;
}

export interface EmptyRow {
  kind: 'empty';
  label: string;
  description: string;
}

export interface UnverifiedRow {
  kind: 'unverified';
  label: string;
  description: string;
}

function slugOf(pageId: string): string {
  const name = pageId.split('/').pop() ?? pageId;
  return name.replace(/\.md$/, '');
}

function statusOf(value: unknown): ComparisonStatus {
  return value === 'requested' || value === 'completed' || value === 'blocked' ? value : 'unknown';
}

/** One row per comparison page the signed-in user can read, in a stable order. */
export function comparisonPageRows(instances: Instance[]): ComparisonPageRow[] {
  return instances
    .map((instance): ComparisonPageRow => {
      const fm = instance.frontmatter;
      const id = typeof fm.id === 'string' && fm.id ? fm.id : slugOf(instance.page_id);
      const experiment = typeof fm.experiment === 'string' ? fm.experiment : '';
      const status = statusOf(fm.status);
      return {
        kind: 'comparison',
        comparison: id,
        label: id,
        // Status first: a narrow view truncates from the right, and the status is what a glance needs.
        description: [status === 'unknown' ? '' : status, experiment].filter(Boolean).join(' · '),
        status,
        experiment,
        ...(typeof fm.result_sha256 === 'string' && fm.result_sha256
          ? { pageResultSha256: fm.result_sha256 }
          : {}),
        ...(typeof fm.reason === 'string' && fm.reason ? { reason: fm.reason } : {}),
      };
    })
    .sort((a, b) => a.comparison.localeCompare(b.comparison));
}

function counts(added: number, removed: number, modified: number): string {
  return [
    modified ? `${modified} modified` : '',
    added ? `${added} added` : '',
    removed ? `${removed} removed` : '',
  ]
    .filter(Boolean)
    .join(' · ');
}

/** Tables of a verified comparison; nothing from Evolve is shown when the page does not match. */
export function tableRows(
  comparison: Comparison,
  verified: boolean,
): (TableRow | EmptyRow | UnverifiedRow)[] {
  if (!verified)
    return [
      {
        kind: 'unverified',
        label: 'Unverified: this page does not match Evolve’s record',
        description: '',
      },
    ];
  if (!comparison.tables.length)
    return [{ kind: 'empty', label: 'No differences from the baseline', description: '' }];
  return [...comparison.tables]
    .sort((a, b) => a.table.localeCompare(b.table))
    .map((t): TableRow => ({
      kind: 'table',
      table: t.table,
      label: t.table,
      description: counts(t.rowsAdded, t.rowsRemoved, t.rowsModified),
    }));
}

export type Side = 'baseline' | 'candidate';

/** `escurel-scenario:/<comparison>/<table>/<side>`; ids and tables are `[A-Za-z0-9_-]`. */
export function comparisonUri(comparison: string, table: string, side: Side): string {
  return `${SCENARIO_SCHEME}:/${encodeURIComponent(comparison)}/${encodeURIComponent(table)}/${side}`;
}

export function parseComparisonUri(
  uri: string,
): { comparison: string; table: string; side: Side } | undefined {
  const prefix = `${SCENARIO_SCHEME}:/`;
  if (!uri.startsWith(prefix)) return undefined;
  const parts = uri.slice(prefix.length).split('/');
  if (parts.length !== 3) return undefined;
  const [comparison, table, side] = parts as [string, string, string];
  if (side !== 'baseline' && side !== 'candidate') return undefined;
  try {
    return { comparison: decodeURIComponent(comparison), table: decodeURIComponent(table), side };
  } catch {
    return undefined;
  }
}

const TOKEN = /^[A-Za-z0-9_-]{1,128}$/;

/** A new comparison request, owned by `owner` and waiting to be computed. */
export function comparisonRequestPage(request: {
  id: string;
  owner: string;
  experiment: string;
  baseline: string;
}): string {
  if (!TOKEN.test(request.id))
    throw new Error('The comparison id must be letters, digits, - or _.');
  if (!TOKEN.test(request.experiment))
    throw new Error('The experiment must be an Evolve experiment id.');
  if (!TOKEN.test(request.baseline))
    throw new Error('The baseline must be seed, parent or a program id.');
  if (!request.owner) throw new Error('Sign in so the comparison has an owner.');
  return [
    '---',
    'kind: instance',
    'skill: evolve_comparison',
    `id: ${request.id}`,
    `owner_subject: ${JSON.stringify(request.owner)}`,
    `experiment: ${request.experiment}`,
    `baseline: ${request.baseline}`,
    'candidate: winner',
    'status: requested',
    'next_comparison_action: evolve_compare',
    '---',
    '',
    `# Comparison of ${request.experiment}`,
    '',
    'Click Compute comparison to ask Evolve what the winner changed relative to the baseline.',
    '',
  ].join('\n');
}

/**
 * Whether the view should look again soon: a comparison is waiting for Evolve to fill it in and
 * someone is looking at the view. Once nothing is waiting, or the view is hidden, it stops.
 */
export function shouldPoll(rows: ComparisonPageRow[], visible: boolean): boolean {
  return visible && rows.some((row) => row.status === 'requested');
}
