import { describe, expect, it } from 'vitest';
import type { LineageNode, ListLineageResponse } from '../../src/client/types';
import { buildInspectors } from '../../src/thread/inspector';
import { focusGraph, layoutThread } from '../../src/thread/layout';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';
import lineage from './fixtures/lineage/lineage-cascade.json';

function recorded() {
  const folded = foldLineage([lineage as ListLineageResponse]);
  const view = toThreadView(folded);
  focusGraph(view, layoutThread(view, new Set()));
  return { view, nodes: [...folded.nodes.values()] };
}

describe('buildInspectors', () => {
  it('shows Evolve validation, admission, and candidate receipts in event inspectors', () => {
    const { view, nodes } = recorded();
    const event = view.nodes.find((node) => node.kind === 'event')!;
    const patched = nodes.map((node) =>
      node.id === event.id
        ? { ...node, label_skill: 'evolve:validation', body: 'Open [[evolve_validation_report::run-1]]' }
        : node,
    );
    const detail = buildInspectors(view, patched)[event.id];
    expect(detail?.body).toBe('Open [[evolve_validation_report::run-1]]');
    expect(detail?.bodyTitle).toBe('Validation evidence');
    const admitted = patched.map((node) => node.id === event.id
      ? { ...node, label_skill: 'evolve:admission',
        body: 'Experiment accepted: [[evolve_experiment::run-1]]' } : node);
    const admission = buildInspectors(view, admitted)[event.id];
    expect(admission?.bodyTitle).toBe('Experiment admission');
    expect(admission?.body).toBe('Experiment accepted: [[evolve_experiment::run-1]]');
    const published = patched.map((node) => node.id === event.id
      ? { ...node, label_skill: 'evolve:candidate',
        body: 'Inactive candidate: [[plan_policy::run-1-7]]' } : node);
    const candidate = buildInspectors(view, published)[event.id];
    expect(candidate?.bodyTitle).toBe('Inactive policy candidate');
    expect(candidate?.body).toBe('Inactive candidate: [[plan_policy::run-1-7]]');
  });
  it('uses recorded run data for rows, summary and timing', () => {
    const { view, nodes } = recorded();
    const run = view.nodes.find((node) => node.kind === 'run')!;
    const detail = buildInspectors(view, nodes)[run.id];
    expect(detail?.title).toBe(run.title);
    expect(detail?.rows).toContainEqual({ k: 'harness', v: 'echo', tech: true });
    expect(detail?.rows).toContainEqual({ k: 'attempts', v: '1/3', tech: true });
    expect(detail?.rows).toContainEqual({
      k: 'trace_id',
      v: '01a0eb194a26f1510f9b86c9f8a6cd96',
      tech: true,
    });
    expect(detail?.bodyTitle).toBe('Summary');
    expect(detail?.body).toContain('awaiting a human');
    expect(detail?.sideTitle).toBe('Timing');
    expect(detail?.side).toContainEqual({ k: 'duration', v: '0 ms' });
    expect(detail?.rows).toContainEqual({ k: 'failed calls', v: '0', tech: true });
    expect(detail?.rows.some((row) => row.k === 'reason')).toBe(false);
  });

  it('lists a recorded draft by target slug and state tone', () => {
    const { view, nodes } = recorded();
    const changeset = view.nodes.find((node) => node.kind === 'changeset')!;
    const detail = buildInspectors(view, nodes)[changeset.id];
    expect(detail?.sideTitle).toBe('Drafts');
    expect(detail?.side).toContainEqual({ k: 'o1', v: 'Applied', tone: 'ok' });
    expect(detail?.rows).toContainEqual({ k: 'Status', v: 'Applied', tone: 'ok' });
  });

  it('omits null values and marks a failed run as an error', () => {
    // The recording has a successful run; this variant covers a failed run with null attributes.
    const { nodes } = recorded();
    const failedNodes = nodes.map((node): LineageNode =>
      node.type === 'run' ? { ...node, state: 'failed', model: null, reason: null } : node,
    );
    const folded = foldLineage([{ root_event_id: lineage.root_event_id, nodes: failedNodes }]);
    const view = toThreadView(folded);
    focusGraph(view, layoutThread(view, new Set()));
    const run = view.nodes.find((node) => node.kind === 'run')!;
    const rows = buildInspectors(view, [...folded.nodes.values()])[run.id]!.rows;
    expect(rows).toContainEqual({ k: 'Status', v: 'Failed', tone: 'error' });
    expect(rows.some((row) => row.k === 'model' || row.k === 'reason')).toBe(false);
  });
});

describe('real data is never dropped for want of its companion', () => {
  // Hand-written: the recording carries both halves of each pair, so it cannot show this.
  const asRun = (extra: Record<string, unknown>) => {
    const { view, nodes } = recorded();
    const run = view.nodes.find((node) => node.kind === 'run')!;
    const patched = nodes.map((n) => (n.id === run.id ? ({ ...n, ...extra } as LineageNode) : n));
    return buildInspectors(view, patched)[run.id]!;
  };

  it('shows attempts when the maximum is absent', () => {
    const detail = asRun({ attempts: 2, max_attempts: null });
    expect(detail.rows).toContainEqual({ k: 'attempts', v: '2', tech: true });
  });

  it('shows the produced page when its version is absent', () => {
    const detail = asRun({
      produced_instance: 'markdown/instances/order__o1.md',
      produced_version: null,
    });
    expect(detail.rows).toContainEqual({
      k: 'produced',
      v: 'markdown/instances/order__o1.md',
      tech: true,
    });
  });
});

describe('labels say what they are', () => {
  it('names the event counts as counts of what is BELOW, not fields of the node', () => {
    // They are derived from the thread that was loaded; the gateway sends no such fields.
    const { view, nodes } = recorded();
    const root = view.nodes.find((n) => n.id === view.rootEventId)!;
    const side = buildInspectors(view, nodes)[root.id]!.side.map((r) => r.k);
    expect(side).toContain('runs below');
    expect(side).toContain('events below');
    expect(side).not.toContain('runs');
  });

  it('does not offer an "Open page" table that is not an action, or repeat the target', () => {
    const { view, nodes } = recorded();
    const draft = view.nodes.find((n) => n.kind === 'draft')!;
    const detail = buildInspectors(view, nodes)[draft.id]!;
    expect(detail.sideTitle).not.toBe('Open page');
    expect(detail.rows.filter((r) => r.k === 'target')).toHaveLength(1);
    expect([...detail.rows, ...detail.side].filter((r) => r.k === 'target_page_id')).toHaveLength(
      0,
    );
  });

  it('does not repeat depth in both tables', () => {
    const { view, nodes } = recorded();
    const hop = view.nodes.find((n) => n.kind === 'event' && n.parent !== null)!;
    const d = buildInspectors(view, nodes)[hop.id]!;
    expect([...d.rows, ...d.side].filter((r) => r.k === 'depth').length).toBeLessThanOrEqual(1);
  });
});

describe('buildInspectors: what a person reads first', () => {
  it('every node opens with a sentence and a kind label, and engineer fields are marked technical', () => {
    const view = toThreadView(foldLineage([lineage as ListLineageResponse]));
    const details = buildInspectors(view, [
      ...foldLineage([lineage as ListLineageResponse]).nodes.values(),
    ]);
    const run = view.nodes.find((n) => n.kind === 'run')!;
    const d = details[run.id]!;
    expect(d.kindLabel).toBe('Agent run');
    expect((d.summary ?? '').length > 10).toBe(true);
    const tech = d.rows.filter((r) => r.tech).map((r) => r.k);
    expect(tech).toContain('trace_id');
    expect(tech).toContain('harness');
    expect(d.rows.find((r) => r.k === 'Status')?.tech).toBeUndefined();
  });
});

describe('node states in words', () => {
  it('says what a state means, not the wire word', async () => {
    const { stateWords } = await import('../../src/thread/inspector');
    expect(stateWords('processed')).toBe('Done');
    expect(stateWords('open')).toBe('Waiting for your review');
    expect(stateWords('dead_letter')).toBe('Gave up');
    expect(stateWords('something_new')).toBe('Something new');
  });
});
