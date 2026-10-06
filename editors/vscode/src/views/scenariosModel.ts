import type { Instance } from '../client/types';
import type { ScenarioDiff } from '../evolve/scenarioDiff';

/** Virtual documents for the baseline-vs-winner diff are served under this scheme. */
export const SCENARIO_SCHEME = 'escurel-scenario';

export interface ExperimentRow {
  kind: 'experiment';
  experiment: string;
  label: string;
  description: string;
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

function slugOf(pageId: string): string {
  const name = pageId.split('/').pop() ?? pageId;
  return name.replace(/\.md$/, '');
}

/** One row per experiment page the signed-in user can read, in a stable order. */
export function experimentRows(instances: Instance[]): ExperimentRow[] {
  return instances
    .map((instance): ExperimentRow => {
      const id =
        typeof instance.frontmatter.id === 'string' && instance.frontmatter.id
          ? instance.frontmatter.id
          : slugOf(instance.page_id);
      const status =
        typeof instance.frontmatter.status === 'string' ? instance.frontmatter.status : '';
      return { kind: 'experiment', experiment: id, label: id, description: status };
    })
    .sort((a, b) => a.experiment.localeCompare(b.experiment));
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

export function tableRows(diff: ScenarioDiff): (TableRow | EmptyRow)[] {
  if (!diff.tables.length)
    return [{ kind: 'empty', label: 'No differences from the seed', description: '' }];
  return [...diff.tables]
    .sort((a, b) => a.table.localeCompare(b.table))
    .map((t): TableRow => ({
      kind: 'table',
      table: t.table,
      label: t.table,
      description: counts(t.rowsAdded, t.rowsRemoved, t.rowsModified),
    }));
}

export type Side = 'seed' | 'winner';

/** `escurel-scenario:/<experiment>/<table>/<side>`; ids and tables are `[A-Za-z0-9_-]`. */
export function scenarioUri(experiment: string, table: string, side: Side): string {
  return `${SCENARIO_SCHEME}:/${encodeURIComponent(experiment)}/${encodeURIComponent(table)}/${side}`;
}

export function parseScenarioUri(
  uri: string,
): { experiment: string; table: string; side: Side } | undefined {
  const prefix = `${SCENARIO_SCHEME}:/`;
  if (!uri.startsWith(prefix)) return undefined;
  const parts = uri.slice(prefix.length).split('/');
  if (parts.length !== 3) return undefined;
  const [experiment, table, side] = parts as [string, string, string];
  if (side !== 'seed' && side !== 'winner') return undefined;
  try {
    return {
      experiment: decodeURIComponent(experiment),
      table: decodeURIComponent(table),
      side,
    };
  } catch {
    return undefined;
  }
}
