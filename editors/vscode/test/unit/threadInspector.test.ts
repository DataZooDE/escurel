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
  it('uses recorded run data for rows, summary and timing', () => {
    const { view, nodes } = recorded();
    const run = view.nodes.find((node) => node.kind === 'run')!;
    const detail = buildInspectors(view, nodes)[run.id];
    expect(detail?.title).toBe(run.title);
    expect(detail?.rows).toContainEqual({ k: 'harness', v: 'echo' });
    expect(detail?.rows).toContainEqual({ k: 'attempts', v: '1/3' });
    expect(detail?.rows).toContainEqual({ k: 'trace_id', v: '01a0eb194a26f1510f9b86c9f8a6cd96' });
    expect(detail?.bodyTitle).toBe('Summary');
    expect(detail?.body).toContain('awaiting a human');
    expect(detail?.sideTitle).toBe('Timing');
    expect(detail?.side).toContainEqual({ k: 'duration', v: '0 ms' });
    expect(detail?.rows).toContainEqual({ k: 'failed calls', v: '0' });
    expect(detail?.rows.some((row) => row.k === 'reason')).toBe(false);
  });

  it('lists a recorded draft by target slug and state tone', () => {
    const { view, nodes } = recorded();
    const changeset = view.nodes.find((node) => node.kind === 'changeset')!;
    const detail = buildInspectors(view, nodes)[changeset.id];
    expect(detail?.sideTitle).toBe('Drafts');
    expect(detail?.side).toContainEqual({ k: 'o1', v: 'promoted', tone: 'ok' });
    expect(detail?.rows).toContainEqual({ k: 'state', v: 'promoted', tone: 'ok' });
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
    expect(rows).toContainEqual({ k: 'state', v: 'failed', tone: 'error' });
    expect(rows.some((row) => row.k === 'model' || row.k === 'reason')).toBe(false);
  });
});
