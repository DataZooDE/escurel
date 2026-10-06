import { describe, expect, it } from 'vitest';
import {
  SCENARIO_SCHEME,
  experimentRows,
  parseScenarioUri,
  scenarioUri,
  tableRows,
} from '../../src/views/scenariosModel';
import { parseScenarioDiff } from '../../src/evolve/scenarioDiff';

const instances = [
  {
    page_id: 'markdown/instances/evolve_experiment/exp-b.md',
    skill: 'evolve_experiment',
    at: '2026-10-01T00:00:00Z',
    frontmatter: { id: 'exp-b', status: 'completed' },
  },
  {
    page_id: 'markdown/instances/evolve_experiment/exp-a.md',
    skill: 'evolve_experiment',
    at: '2026-10-01T00:00:00Z',
    frontmatter: { id: 'exp-a', status: 'running' },
  },
  {
    page_id: 'markdown/instances/evolve_experiment/odd.md',
    skill: 'evolve_experiment',
    at: '2026-10-01T00:00:00Z',
    frontmatter: {},
  },
];

describe('experimentRows', () => {
  it('lists experiments by id, sorted, using the page slug when the frontmatter has no id', () => {
    const rows = experimentRows(instances);
    expect(rows.map((r) => r.experiment)).toEqual(['exp-a', 'exp-b', 'odd']);
  });

  it('shows the status so a running experiment is not mistaken for a finished one', () => {
    const rows = experimentRows(instances);
    expect(rows.find((r) => r.experiment === 'exp-a')?.description).toBe('running');
  });
});

describe('tableRows', () => {
  const diff = parseScenarioDiff({
    experiment: 'exp-a',
    pilot: 'p0',
    baseline_program_id: 1,
    winner_program_id: 2,
    tables: [
      { table: 'p0_bin_assignment', rows_added: 0, rows_removed: 1, rows_modified: 2 },
      { table: 'a_table', rows_added: 3, rows_removed: 0, rows_modified: 0 },
    ],
    rows: [],
    truncated: false,
    evidence_note: '',
  });

  it('lists tables with exact counts, sorted by name', () => {
    const rows = tableRows(diff);
    expect(rows.map((r) => (r.kind === 'table' ? r.table : ''))).toEqual([
      'a_table',
      'p0_bin_assignment',
    ]);
    expect(rows.map((r) => r.description)).toEqual(['3 added', '2 modified · 1 removed']);
  });

  it('says so when the winner changed nothing', () => {
    const empty = parseScenarioDiff({
      ...{ experiment: 'x', winner_program_id: 2 },
      tables: [],
      rows: [],
    });
    expect(tableRows(empty)).toEqual([
      { kind: 'empty', label: 'No differences from the seed', description: '' },
    ]);
  });
});

describe('scenario URIs', () => {
  it('round-trips an experiment, table and side', () => {
    const uri = scenarioUri('exp-a', 'p0_bin_assignment', 'winner');
    expect(uri.startsWith(`${SCENARIO_SCHEME}:`)).toBe(true);
    expect(parseScenarioUri(uri)).toEqual({
      experiment: 'exp-a',
      table: 'p0_bin_assignment',
      side: 'winner',
    });
  });

  it('survives characters that are legal in an experiment id', () => {
    const uri = scenarioUri('p0-e2e_1', 't', 'seed');
    expect(parseScenarioUri(uri)?.experiment).toBe('p0-e2e_1');
  });

  it('refuses a uri it did not make', () => {
    expect(parseScenarioUri('file:///etc/passwd')).toBeUndefined();
    expect(parseScenarioUri(`${SCENARIO_SCHEME}:/a/b/c/d`)).toBeUndefined();
    expect(parseScenarioUri(`${SCENARIO_SCHEME}:/exp/t/middle`)).toBeUndefined();
  });
});
