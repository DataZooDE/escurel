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

describe('outline rows carry what the tree needs to show them truthfully', () => {
  const rowsOf = () => {
    const folded = foldLineage([lineage as ListLineageResponse]);
    return outlineRows(toThreadView(folded), new Set());
  };
  const flat = (rs: ReturnType<typeof rowsOf>): ReturnType<typeof rowsOf> =>
    rs.flatMap((r) => [r, ...flat(r.children)]);

  it('keeps the node state, so a running run is not shown as a passed one', () => {
    // The tree coloured a run green unless its description said it had failed, so a running
    // or planned run looked like a success. The state has to travel with the row.
    const run = flat(rowsOf()).find((r) => r.contextValue === 'escurel.run')!;
    expect(run.state).toBe('processed');
    expect(flat(rowsOf()).every((r) => 'state' in r)).toBe(true);
  });

  it('keeps the node kind, so each row can have its own icon', () => {
    const kinds = new Set(flat(rowsOf()).map((r) => r.kind));
    expect(kinds).toEqual(new Set(['event', 'run', 'changeset', 'draft']));
  });
});
