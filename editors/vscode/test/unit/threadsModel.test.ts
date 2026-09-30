import { describe, expect, it } from 'vitest';
import type { ListLineageResponse } from '../../src/client/types';
import { focusGraph, layoutThread } from '../../src/thread/layout';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';
import { outlineRows } from '../../src/views/threadsModel';
import lineage from './fixtures/lineage/lineage-cascade.json';

function recorded() {
  const view = toThreadView(foldLineage([lineage as ListLineageResponse]));
  focusGraph(view, layoutThread(view, new Set()));
  return view;
}

describe('outlineRows', () => {
  it('nests the recorded run, changeset, draft and cascade event under the root', () => {
    const view = recorded();
    const [root] = outlineRows(view, new Set());
    expect(root?.id).toBe(view.rootEventId);
    expect(root?.contextValue).toBe('escurel.event');
    expect(root?.label).toBe(view.nodes.find((node) => node.id === root?.id)?.title);
    const run = root?.children[0];
    expect(run?.contextValue).toBe('escurel.run');
    expect(run?.target).toEqual({ open: 'run', runId: run?.id });
    expect(run?.description).toBe('processed');
    expect(run?.children.map((row) => row.contextValue)).toEqual([
      'escurel.event',
      'escurel.changeset',
    ]);
    expect(run?.children[1]?.children[0]?.contextValue).toBe('escurel.draft');
  });

  it('marks a collapsed run so its descendants are hidden by the tree', () => {
    const view = recorded();
    const runId = view.nodes.find((node) => node.kind === 'run')!.id;
    const run = outlineRows(view, new Set([runId]))[0]!.children[0]!;
    expect(run.collapsibleState).toBe('collapsed');
    expect(run.children).toHaveLength(2);
    expect(outlineRows(view, new Set())[0]!.children[0]!.collapsibleState).toBe('expanded');
  });
});
