import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { ThreadNode, ThreadView } from '../../src/shared/protocol';
import { CARD_WIDTH, focusGraph, layoutThread } from '../../src/thread/layout';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';

interface LineageFixtureNode {
  id: string;
  type: 'event' | 'run' | 'changeset' | 'draft';
  parent: string | null;
  state: string | null;
  title?: string;
}

interface LineageFixture {
  root_event_id: string;
  nodes: LineageFixtureNode[];
}

function loadLineageFixture(filename: string): LineageFixture {
  const filePath = join(__dirname, 'fixtures', 'lineage', filename);
  return JSON.parse(readFileSync(filePath, 'utf8')) as LineageFixture;
}

/**
 * Derives a valid ThreadView from a recorded gateway lineage fixture.
 * Children arrays are populated from parent references to reflect the
 * gateway's hierarchy, while nodes retain their recorded states and ids.
 */
function threadViewFromFixture(fixture: LineageFixture): ThreadView {
  const childrenMap = new Map<string, string[]>();
  for (const node of fixture.nodes) {
    if (node.parent) {
      const existing = childrenMap.get(node.parent) ?? [];
      existing.push(node.id);
      childrenMap.set(node.parent, existing);
    }
  }

  const nodes: ThreadNode[] = fixture.nodes.map((node) => ({
    id: node.id,
    kind: node.type,
    parent: node.parent,
    children: childrenMap.get(node.id) ?? [],
    state: node.state,
    tone: 'neutral',
    title: node.title ?? node.id,
    meta: [],
    chips: [],
    target: { open: 'nothing' },
    collapsible: node.type === 'run' || (childrenMap.get(node.id)?.length ?? 0) > 0,
  }));

  return {
    rootEventId: fixture.root_event_id,
    nodes,
    columns: [
      'root event',
      'run · changeset',
      'instances · drafts',
      'cascade · depth 1',
      'outbound · depth 2',
    ],
    loadingMore: false,
  };
}

// Hand-written nodes cover a second cascade generation absent from the recordings.
function lineageNode(
  id: string,
  kind: ThreadNode['kind'],
  parent: string | null,
  children: string[],
): ThreadNode {
  return {
    id,
    kind,
    parent,
    children,
    state: 'processed',
    tone: 'neutral',
    title: id,
    meta: [],
    chips: [],
    target: { open: 'nothing' },
    collapsible: kind === 'run' || children.length > 0,
  };
}

describe('thread layout and focus graph', () => {
  it('places an event directly under the root in column 1', () => {
    // Hand-written input: the recordings have no event whose parent is the root.
    const view: ThreadView = {
      rootEventId: 'root',
      nodes: [
        lineageNode('root', 'event', null, ['child']),
        lineageNode('child', 'event', 'root', []),
      ],
      columns: ['root event'],
      loadingMore: false,
    };

    expect(layoutThread(view, new Set()).nodes.find((node) => node.id === 'child')?.column).toBe(1);
  });

  it('labels every used column across two cascade generations', () => {
    // Hand-written input: recordings contain only one cascade generation.
    const view: ThreadView = {
      rootEventId: 'root',
      nodes: [
        lineageNode('root', 'event', null, ['run1']),
        lineageNode('run1', 'run', 'root', ['cs1', 'hop1']),
        lineageNode('cs1', 'changeset', 'run1', ['draft1']),
        lineageNode('draft1', 'draft', 'cs1', []),
        lineageNode('hop1', 'event', 'run1', ['run2']),
        lineageNode('run2', 'run', 'hop1', ['cs2']),
        lineageNode('cs2', 'changeset', 'run2', ['draft2']),
        lineageNode('draft2', 'draft', 'cs2', ['hop2']),
        lineageNode('hop2', 'event', 'draft2', []),
      ],
      columns: [
        'root event',
        'run · changeset',
        'instances · drafts',
        'cascade · depth 1',
        'outbound · depth 2',
      ],
      loadingMore: false,
    };

    const layout = layoutThread(view, new Set());
    const usedColumns = new Set(
      layout.nodes.filter((node) => !node.hidden).map((node) => node.column),
    );
    expect(layout.columnHeaders).toHaveLength(usedColumns.size);
    expect(layout.columnHeaders.every((header) => header.label.trim().length > 0)).toBe(true);
    expect(layout.nodes.find((node) => node.id === 'hop2')?.column).toBe(6);
    expect(layout.columnHeaders.at(-2)?.label).toBe('drafts · depth 2');
    expect(layout.columnHeaders.at(-1)?.label).toBe('cascade · depth 2');
  });

  it('keeps layout and focus identical when flat nodes are reversed or rotated', () => {
    const view = threadViewFromFixture(loadLineageFixture('lineage-cascade.json'));
    const baseline = layoutThread(view, new Set());
    const baselineFocus = focusGraph(view, baseline);
    for (const nodes of [
      [...view.nodes].reverse(),
      [...view.nodes.slice(2), ...view.nodes.slice(0, 2)],
    ]) {
      const shuffledView = { ...view, nodes };
      const shuffledLayout = layoutThread(shuffledView, new Set());
      expect(shuffledLayout).toEqual(baseline);
      expect(focusGraph(shuffledView, shuffledLayout)).toEqual(baselineFocus);
    }
  });

  it('models collapsible parents and all five mock column labels in recorded views', () => {
    const view = threadViewFromFixture(loadLineageFixture('lineage-cascade.json'));
    expect(
      view.nodes.every(
        (node) => node.collapsible === (node.kind === 'run' || node.children.length > 0),
      ),
    ).toBe(true);
    expect(view.columns).toEqual([
      'root event',
      'run · changeset',
      'instances · drafts',
      'cascade · depth 1',
      'outbound · depth 2',
    ]);
  });
  // Test 1: Columns for the cascade shape
  it('assigns columns for cascade shape: root 0, run 1, changeset 1, draft 2, cascade event 3', () => {
    const fixture = loadLineageFixture('lineage-cascade.json');
    const view = threadViewFromFixture(fixture);

    const layout = layoutThread(view, new Set());

    const root = layout.nodes.find((n) => n.id === '01M3NHJJCWP2TXH9TQT46F70R1');
    const run = layout.nodes.find((n) => n.id === '01M3NHJJGJ1WWAV26F5Z8Y4XKT');
    const changeset = layout.nodes.find((n) => n.id === '01M3NHJJNJ7P58Q9A7PWEYSA9X');
    const draft = layout.nodes.find((n) => n.id === '01M3NHJJNME9ERH58A5BEWPKDP');
    const cascade = layout.nodes.find((n) => n.id === 'cascade:01M3NHJJNME9ERH58A5BEWPKDP');

    expect(root?.column).toBe(0);
    expect(run?.column).toBe(1);
    expect(changeset?.column).toBe(1);
    expect(draft?.column).toBe(2);
    expect(cascade?.column).toBe(3);
  });

  // Test 2: Collapsing the run hides descendants, takes no space, suppresses wires, strips focus
  it('collapsing the run hides changeset, draft and cascade event with no space, wires, or focus', () => {
    const fixture = loadLineageFixture('lineage-cascade.json');
    const view = threadViewFromFixture(fixture);

    const uncollapsed = layoutThread(view, new Set());
    const runId = '01M3NHJJGJ1WWAV26F5Z8Y4XKT';
    const collapsed = layoutThread(view, new Set([runId]));

    const changeset = collapsed.nodes.find((n) => n.id === '01M3NHJJNJ7P58Q9A7PWEYSA9X');
    const draft = collapsed.nodes.find((n) => n.id === '01M3NHJJNME9ERH58A5BEWPKDP');
    const cascade = collapsed.nodes.find((n) => n.id === 'cascade:01M3NHJJNME9ERH58A5BEWPKDP');
    const run = collapsed.nodes.find((n) => n.id === runId);

    // Descendants are marked hidden and take no spatial dimensions
    expect(changeset?.hidden).toBe(true);
    expect(draft?.hidden).toBe(true);
    expect(cascade?.hidden).toBe(true);
    expect(changeset?.width).toBe(0);
    expect(draft?.width).toBe(0);
    expect(cascade?.width).toBe(0);

    // Collapsed node itself remains visible
    expect(run?.hidden).toBe(false);
    expect(run?.width).toBe(CARD_WIDTH);

    // Layout bounds shrink because hidden cards take no space
    expect(collapsed.bounds.width).toBeLessThan(uncollapsed.bounds.width);

    // No wires lead to or from hidden descendants
    const hiddenIds = new Set([changeset!.id, draft!.id, cascade!.id]);
    for (const wire of collapsed.wires) {
      expect(hiddenIds.has(wire.from)).toBe(false);
      expect(hiddenIds.has(wire.to)).toBe(false);
    }

    // Focus graph omits hidden nodes and drops next step from run
    const focus = focusGraph(view, collapsed);
    expect(focus.steps[changeset!.id]).toBeUndefined();
    expect(focus.steps[draft!.id]).toBeUndefined();
    expect(focus.steps[cascade!.id]).toBeUndefined();
    expect(focus.steps[runId]?.next).toBeUndefined();
  });

  // Test 3: Wire styles: promoted draft is promoted, processed cascade event is sent
  it('marks wire style promoted for promoted draft and sent for processed cascade event', () => {
    const fixture = loadLineageFixture('lineage-cascade.json');
    const view = threadViewFromFixture(fixture);

    // Hand-crafted case adjustment: ensure cascade event state is processed to test sent wire style
    const cascadeNode = view.nodes.find((n) => n.id === 'cascade:01M3NHJJNME9ERH58A5BEWPKDP')!;
    cascadeNode.state = 'processed';

    const layout = layoutThread(view, new Set());

    const draftWire = layout.wires.find((w) => w.to === '01M3NHJJNME9ERH58A5BEWPKDP');
    const cascadeWire = layout.wires.find((w) => w.to === 'cascade:01M3NHJJNME9ERH58A5BEWPKDP');

    expect(draftWire?.style).toBe('promoted');
    expect(cascadeWire?.style).toBe('sent');
  });

  // Test 4: Focus directions
  it('builds focus graph where → reaches run, ← reaches changeset, ↓ reaches changeset', () => {
    const fixture = loadLineageFixture('lineage-cascade.json');
    const view = threadViewFromFixture(fixture);

    const layout = layoutThread(view, new Set());
    const focus = focusGraph(view, layout);

    const rootId = '01M3NHJJCWP2TXH9TQT46F70R1';
    const runId = '01M3NHJJGJ1WWAV26F5Z8Y4XKT';
    const changesetId = '01M3NHJJNJ7P58Q9A7PWEYSA9X';
    const draftId = '01M3NHJJNME9ERH58A5BEWPKDP';

    // → from the root reaches the run
    expect(focus.steps[rootId]?.next).toBe(runId);

    // ← from the draft reaches the changeset
    expect(focus.steps[draftId]?.back).toBe(changesetId);

    // ↓ from the run reaches the changeset (same column, below)
    expect(focus.steps[runId]?.down).toBe(changesetId);
  });

  // Test 5: Determinism
  it('produces identical layout and focus graph when run twice on the same input', () => {
    const fixture = loadLineageFixture('lineage-cascade.json');
    const view = threadViewFromFixture(fixture);

    const layout1 = layoutThread(view, new Set());
    const layout2 = layoutThread(view, new Set());

    expect(layout1).toEqual(layout2);

    const focus1 = focusGraph(view, layout1);
    const focus2 = focusGraph(view, layout2);

    expect(focus1).toEqual(focus2);
  });

  // Test 6: Every wire path starts at parent right edge and ends at child left edge
  it('starts every wire at its parents right edge and ends at its childs left edge', () => {
    const fixture = loadLineageFixture('lineage-cascade.json');
    const view = threadViewFromFixture(fixture);

    const layout = layoutThread(view, new Set());
    const nodeMap = new Map(layout.nodes.map((n) => [n.id, n]));

    expect(layout.wires.length).toBeGreaterThan(0);

    for (const wire of layout.wires) {
      const parent = nodeMap.get(wire.from)!;
      const child = nodeMap.get(wire.to)!;

      expect(parent).toBeDefined();
      expect(child).toBeDefined();

      // Expected parent right-middle and child left-middle
      const expectedStartX = parent.x + parent.width;
      const expectedStartY = parent.y + parent.height / 2;
      const expectedEndX = child.x;
      const expectedEndY = child.y + child.height / 2;

      // Parse M x y and final coordinates out of path: "M x1 y1 C ... x2 y2"
      const match = /^M\s*([0-9.-]+)\s+([0-9.-]+)\s+C.*?\s+([0-9.-]+)\s+([0-9.-]+)$/.exec(
        wire.path,
      );
      expect(match, `Invalid wire path format: ${wire.path}`).not.toBeNull();
      const sx = match?.[1];
      const sy = match?.[2];
      const ex = match?.[3];
      const ey = match?.[4];
      expect(sx).toBeDefined();
      expect(sy).toBeDefined();
      expect(ex).toBeDefined();
      expect(ey).toBeDefined();

      const startX = parseFloat(sx ?? '0');
      const startY = parseFloat(sy ?? '0');
      const endX = parseFloat(ex ?? '0');
      const endY = parseFloat(ey ?? '0');

      expect(startX).toBeCloseTo(expectedStartX, 2);
      expect(startY).toBeCloseTo(expectedStartY, 2);
      expect(endX).toBeCloseTo(expectedEndX, 2);
      expect(endY).toBeCloseTo(expectedEndY, 2);
    }
  });

  // Additional edge case tests: hand-written inputs for cases recordings cannot show
  it('stacks multiple drafts in column 2 and sets up/down focus steps between them', () => {
    // Hand-written input: 1 root, 1 run, 1 changeset with 2 drafts
    const view: ThreadView = {
      rootEventId: 'e-root',
      loadingMore: false,
      columns: ['root', 'run', 'drafts'],
      nodes: [
        {
          id: 'e-root',
          kind: 'event',
          parent: null,
          children: ['r1'],
          state: 'processed',
          tone: 'event',
          title: 'Root',
          meta: [],
          chips: [],
          target: { open: 'nothing' },
          collapsible: false,
        },
        {
          id: 'r1',
          kind: 'run',
          parent: 'e-root',
          children: ['cs1'],
          state: 'processed',
          tone: 'run',
          title: 'Run 1',
          meta: [],
          chips: [],
          target: { open: 'nothing' },
          collapsible: true,
        },
        {
          id: 'cs1',
          kind: 'changeset',
          parent: 'r1',
          children: ['d1', 'd2'],
          state: 'promoted',
          tone: 'neutral',
          title: 'CS 1',
          meta: [],
          chips: [],
          target: { open: 'nothing' },
          collapsible: false,
        },
        {
          id: 'd1',
          kind: 'draft',
          parent: 'cs1',
          children: [],
          state: 'promoted',
          tone: 'instance',
          title: 'Draft 1',
          meta: [],
          chips: [],
          target: { open: 'nothing' },
          collapsible: false,
        },
        {
          id: 'd2',
          kind: 'draft',
          parent: 'cs1',
          children: [],
          state: 'promoted',
          tone: 'instance',
          title: 'Draft 2',
          meta: [],
          chips: [],
          target: { open: 'nothing' },
          collapsible: false,
        },
      ],
    };

    const layout = layoutThread(view, new Set());
    const d1 = layout.nodes.find((n) => n.id === 'd1')!;
    const d2 = layout.nodes.find((n) => n.id === 'd2')!;

    expect(d1.column).toBe(2);
    expect(d2.column).toBe(2);
    expect(d2.y).toBeGreaterThan(d1.y);

    const focus = focusGraph(view, layout);
    expect(focus.steps['d1']?.down).toBe('d2');
    expect(focus.steps['d2']?.up).toBe('d1');
  });

  it('handles empty thread view gracefully with zero bounds and empty focus graph', () => {
    // Hand-written input: unknown root yields empty node list
    const view: ThreadView = {
      rootEventId: 'e-empty',
      loadingMore: false,
      columns: ['root'],
      nodes: [],
    };

    const layout = layoutThread(view, new Set());
    expect(layout.nodes).toHaveLength(0);
    expect(layout.wires).toHaveLength(0);
    expect(layout.bounds).toEqual({ width: 0, height: 0 });

    const focus = focusGraph(view, layout);
    expect(focus.first).toBe('e-empty');
    expect(Object.keys(focus.steps)).toHaveLength(0);
  });
});

describe('layout over the real model', () => {
  // The tests above build a ThreadView by hand, which can drift from what the model emits.
  // This one runs the recorded cascade through `foldLineage` and `toThreadView` and lays out
  // the result, so a model change that breaks the layout contract fails here.
  it('lays out and navigates the recorded cascade end to end', () => {
    const file = join(__dirname, 'fixtures', 'lineage', 'lineage-cascade.json');
    const view = toThreadView(foldLineage([JSON.parse(readFileSync(file, 'utf8'))]));
    const layout = layoutThread(view, new Set());
    const focus = focusGraph(view, layout);

    // Every node the gateway returned is on the canvas, reachable, and every column used
    // has a header with text.
    expect(layout.nodes.filter((n) => !n.hidden)).toHaveLength(view.nodes.length);
    for (const header of layout.columnHeaders) expect(header.label).not.toBe('');
    expect(Object.keys(focus.steps)).toHaveLength(view.nodes.length);

    // Walking → from the root reaches the run, the recorded tree's second level.
    const run = view.nodes.find((n) => n.kind === 'run')!;
    expect(focus.steps[focus.first]?.next).toBe(run.id);
    // A cascade hop is to the right of the run that produced it.
    const hop = view.nodes.find((n) => n.kind === 'event' && n.parent === run.id)!;
    const col = (id: string) => layout.nodes.find((n) => n.id === id)!.column;
    expect(col(hop.id)).toBeGreaterThan(col(run.id));
  });
});
