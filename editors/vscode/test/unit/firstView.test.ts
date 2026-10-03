import { describe, expect, it } from 'vitest';
import {
  LOW_ZOOM_BELOW,
  firstViewport,
  isLowZoom,
  pickTarget,
  scrollMetrics,
} from '../../src/thread/firstView';
import {
  branchingLayout,
  branchingThreadView,
  openChangesetLayout,
  openChangesetThreadView,
  recordedLayout,
  recordedThreadView,
} from '../component/thread-fixtures';
import type { ThreadLayout, ThreadNode, ThreadView } from '../../src/shared/protocol';

// First view: bring the node that matters into view, once. What matters: the first node that waits on
// a person (main row first, then lanes top to bottom, left to right); else the newest active node; else
// the last node of the main row.
describe('pickTarget', () => {
  it('is the first node that needs you, in document order', () => {
    const wanting = branchingThreadView.nodes.filter((n) => n.needsYou).map((n) => n.id);
    expect(wanting.length > 1, 'the branching fixture has several waiting nodes').toBe(true);
    const target = pickTarget(branchingThreadView, branchingLayout)!;
    expect(wanting).toContain(target);
    // Document order: by lane (top to bottom), then left to right.
    const byId = new Map(branchingLayout.nodes.map((n) => [n.id, n]));
    const laneOf = (id: string) => {
      const n = byId.get(id)!;
      let lane = 0;
      branchingLayout.lanes.forEach((l, i) => {
        if (n.y + n.height / 2 >= l.y) lane = i;
      });
      return lane;
    };
    for (const other of wanting) {
      const a = [laneOf(target), byId.get(target)!.x];
      const b = [laneOf(other), byId.get(other)!.x];
      expect(a[0]! < b[0]! || (a[0] === b[0] && a[1]! <= b[1]!)).toBe(true);
    }
  });

  it('prefers the main row over a lane below it, whatever the ids say', () => {
    const view = branchingThreadView;
    const lanes = branchingLayout.lanes;
    expect(lanes.length >= 2).toBe(true);
    const target = pickTarget(view, branchingLayout)!;
    const node = branchingLayout.nodes.find((n) => n.id === target)!;
    expect(node.y < lanes[1]!.y).toBe(true);
  });

  it('with nobody waiting, is the newest active node (newest id among unfinished)', () => {
    const calm: ThreadView = {
      ...branchingThreadView,
      nodes: branchingThreadView.nodes.map((n) => {
        const { needsYou: _gone, ...rest } = n;
        void _gone;
        return { ...rest, emphasis: n.emphasis === 'needs-you' ? ('normal' as const) : n.emphasis };
      }) as ThreadNode[],
    };
    const active = calm.nodes.filter((n) => n.emphasis !== 'compact');
    const newest = [...active].sort((a, b) => (a.id < b.id ? 1 : -1))[0]!;
    expect(pickTarget(calm, branchingLayout)).toBe(newest.id);
  });

  it('with everything finished, is the last node of the main row', () => {
    const done: ThreadView = {
      ...recordedThreadView,
      nodes: recordedThreadView.nodes.map((n) => {
        const { needsYou: _gone, ...rest } = n;
        void _gone;
        return { ...rest, emphasis: 'compact' as const };
      }) as ThreadNode[],
    };
    const main = recordedLayout.nodes
      .filter(
        (n) => !n.hidden && (recordedLayout.lanes[1] ? n.y < recordedLayout.lanes[1].y : true),
      )
      .sort((a, b) => a.x - b.x || a.y - b.y);
    expect(pickTarget(done, recordedLayout)).toBe(main[main.length - 1]!.id);
  });

  it('ignores nodes hidden by a collapsed ancestor', () => {
    const layout: ThreadLayout = {
      ...openChangesetLayout,
      nodes: openChangesetLayout.nodes.map((n) => ({ ...n, hidden: true })),
    };
    expect(pickTarget(openChangesetThreadView, layout)).toBeUndefined();
  });
});

const box = (w: number, h: number) => ({ width: w, height: h });

describe('firstViewport', () => {
  const layout = (bw: number, bh: number, x: number, y: number, w = 200, h = 80) =>
    ({
      nodes: [{ id: 't', column: 0, x, y, width: w, height: h, hidden: false }],
      wires: [],
      bounds: { width: bw, height: bh },
      columnHeaders: [],
      lanes: [],
    }) as ThreadLayout;

  it('a graph that fits at 100% opens as is: 100%, no scroll', () => {
    expect(firstViewport(layout(800, 300, 600, 100), 't', box(1200, 700))).toEqual({
      x: 0,
      y: 0,
      zoom: 1,
    });
  });

  it('a wider graph opens at 100% with the target centred', () => {
    const v = firstViewport(layout(3000, 300, 2000, 100), 't', box(1000, 700));
    expect(v.zoom).toBe(1);
    // target centre (2100, 140) lands at the container centre (500, ...): x = 500 - 2100
    expect(v.x).toBe(-1600);
  });

  it('is clamped to the graph: a target near the right edge does not scroll past it', () => {
    const v = firstViewport(layout(3000, 300, 2800, 100), 't', box(1000, 700));
    expect(v.x).toBe(1000 - 3000);
  });

  it('is clamped on the left: a target near the start stays at the start', () => {
    expect(firstViewport(layout(3000, 300, 10, 100), 't', box(1000, 700)).x).toBe(0);
  });

  it('scrolls vertically too when the graph is taller than the canvas', () => {
    const v = firstViewport(layout(800, 2000, 100, 1500), 't', box(1200, 600));
    expect(v.y).toBe(-(1540 - 300));
  });

  it('treats an unknown (0) container height as unknown, not as overflow', () => {
    expect(firstViewport(layout(800, 300, 600, 100), 't', box(1200, 0)).y).toBe(0);
  });

  it('without a target it opens at the start', () => {
    expect(firstViewport(layout(3000, 300, 2000, 100), undefined, box(1000, 700))).toEqual({
      x: 0,
      y: 0,
      zoom: 1,
    });
  });
});

describe('semantic zoom threshold', () => {
  it('switches to the low-zoom form below 70%, and only below', () => {
    expect(LOW_ZOOM_BELOW).toBe(0.7);
    expect(isLowZoom(0.69)).toBe(true);
    expect(isLowZoom(0.7)).toBe(false);
    expect(isLowZoom(1)).toBe(false);
  });
});

// A viewport that clips the graph needs a visible way to move: thumbs sized by the visible share.
describe('scrollMetrics', () => {
  it('has no thumb for an axis that fits', () => {
    expect(
      scrollMetrics({ x: 0, y: 0, zoom: 1 }, { width: 800, height: 300 }, box(1000, 700)),
    ).toEqual({ h: undefined, v: undefined });
  });

  it('sizes and places the horizontal thumb by what is visible', () => {
    const m = scrollMetrics(
      { x: -500, y: 0, zoom: 1 },
      { width: 2000, height: 300 },
      box(1000, 700),
    );
    expect(m.h).toEqual({ size: 0.5, pos: 0.25 });
    expect(m.v).toBeUndefined();
  });

  it('accounts for zoom', () => {
    const m = scrollMetrics(
      { x: 0, y: 0, zoom: 0.5 },
      { width: 4000, height: 300 },
      box(1000, 700),
    );
    expect(m.h).toEqual({ size: 0.5, pos: 0 });
  });
});
