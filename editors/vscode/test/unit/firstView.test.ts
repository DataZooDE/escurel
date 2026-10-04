import { describe, expect, it } from 'vitest';
import {
  LOW_ZOOM_BELOW,
  columnsOffRight,
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
// a person (main row first, then lanes top to bottom, left to right); when NOTHING needs the person the
// thread opens at its ROOT, where the story starts (the owner found a thread opening scrolled to its
// newest node, with the beginning off screen, disorienting).
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

  const plainNode = (id: string, emphasis: 'compact' | 'normal', extra = {}) =>
    ({
      id,
      kind: 'event',
      parent: null,
      children: [],
      state: 'x',
      tone: 'neutral',
      title: id,
      meta: [],
      chips: [],
      target: { kind: 'none' },
      collapsible: false,
      emphasis,
      ...extra,
    }) as unknown as ThreadNode;
  const laidAt = (id: string, x: number, hidden = false) => ({
    id,
    column: 0,
    x,
    y: 0,
    width: 100,
    height: 50,
    hidden,
  });
  const layoutOf = (nodes: ReturnType<typeof laidAt>[]) =>
    ({
      nodes,
      wires: [],
      bounds: { width: 600, height: 100 },
      columnHeaders: [],
      lanes: [],
    }) as ThreadLayout;

  it('with nobody waiting, is the ROOT of the thread, even when newer unfinished nodes exist', () => {
    const view = {
      rootEventId: '01A',
      nodes: [plainNode('01A', 'compact'), plainNode('01B', 'normal'), plainNode('01C', 'normal')],
      columns: [],
      loading: false,
    } as unknown as ThreadView;
    const layout = layoutOf([laidAt('01A', 0), laidAt('01B', 200), laidAt('01C', 400)]);
    expect(pickTarget(view, layout)).toBe('01A');
  });

  it('with everything finished, is still the root', () => {
    const done: ThreadView = {
      ...recordedThreadView,
      nodes: recordedThreadView.nodes.map((n) => {
        const { needsYou: _gone, ...rest } = n;
        void _gone;
        return { ...rest, emphasis: 'compact' as const };
      }) as ThreadNode[],
    };
    expect(pickTarget(done, recordedLayout)).toBe(recordedThreadView.rootEventId);
  });

  it('a node that needs you still wins over the root', () => {
    const view = {
      rootEventId: '01A',
      nodes: [plainNode('01A', 'compact'), plainNode('01B', 'normal', { needsYou: true })],
      columns: [],
      loading: false,
    } as unknown as ThreadView;
    const layout = layoutOf([laidAt('01A', 0), laidAt('01B', 200)]);
    expect(pickTarget(view, layout)).toBe('01B');
  });

  it('falls back to the first visible node when the root is hidden or unknown', () => {
    const view = {
      rootEventId: '01A',
      nodes: [plainNode('01A', 'compact'), plainNode('01B', 'normal')],
      columns: [],
      loading: false,
    } as unknown as ThreadView;
    const layout = layoutOf([laidAt('01A', 0, true), laidAt('01B', 200)]);
    expect(pickTarget(view, layout)).toBe('01B');
    expect(
      pickTarget({ ...view, rootEventId: 'nope' } as ThreadView, layoutOf([laidAt('01B', 200)])),
    ).toBe('01B');
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

  it('a wider graph opens at 100% with the target at the left, with a margin (not centred, so no card is cut at the left edge)', () => {
    const v = firstViewport(layout(3000, 300, 2000, 100), 't', box(1000, 700));
    expect(v.zoom).toBe(1);
    expect(v.x).toBe(-(2000 - 24));
  });

  describe('columns: the card to the left of the target is in view, whole', () => {
    const columns = (xs: number[]) =>
      ({
        nodes: xs.map((x, i) => ({
          id: `c${i}`,
          column: i,
          x,
          y: 40,
          width: 240,
          height: 80,
          hidden: false,
        })),
        wires: [],
        bounds: { width: xs[xs.length - 1]! + 240, height: 300 },
        columnHeaders: [],
        lanes: [],
      }) as ThreadLayout;
    const straddlesLeft = (l: ThreadLayout, x: number) =>
      l.nodes.filter((n) => n.x + x < 0 && n.x + n.width + x > 0).map((n) => n.id);

    it('starts at the left neighbour column when both fit, so nothing is cut at the left edge', () => {
      const l = columns([0, 290, 580, 870, 1160, 1450, 1740]);
      const v = firstViewport(l, 'c3', box(1000, 700));
      expect(v.x).toBe(-(580 - 24));
      expect(straddlesLeft(l, v.x)).toEqual([]);
      // the neighbour and the target are both fully visible
      for (const id of ['c2', 'c3']) {
        const n = l.nodes.find((c) => c.id === id)!;
        expect(n.x + v.x >= 0 && n.x + n.width + v.x <= 1000).toBe(true);
      }
    });

    it('starts at the target itself when the neighbour and the target do not fit together', () => {
      const l = columns([0, 290, 580, 870, 1160, 1450, 1740]);
      const v = firstViewport(l, 'c3', box(400, 700));
      expect(v.x).toBe(-(870 - 24));
      expect(straddlesLeft(l, v.x)).toEqual([]);
    });
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
  it('switches to the low-zoom form below 85%, and only below', () => {
    expect(LOW_ZOOM_BELOW).toBe(0.85);
    expect(isLowZoom(0.84)).toBe(true);
    expect(isLowZoom(0.7)).toBe(true);
    expect(isLowZoom(0.85)).toBe(false);
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

describe('scrollMetrics: the thumb stays on its track', () => {
  it('never starts past 1 - size, even when the view is panned beyond the graph', () => {
    const m = scrollMetrics(
      { x: -9000, y: 0, zoom: 1 },
      { width: 2000, height: 300 },
      box(1000, 700),
    );
    expect(m.h!.pos).toBe(0.5);
    expect(m.h!.pos + m.h!.size <= 1).toBe(true);
  });
});

// The graph is wider than the window and nothing said so: a cut-off edge with no cue that more exists.
describe('columnsOffRight', () => {
  const nodes = [0, 290, 580, 870, 1160].map((x, i) => ({
    id: `c${i}`,
    column: i,
    x,
    y: 0,
    width: 240,
    height: 80,
    hidden: false,
  }));
  const layout = {
    nodes,
    wires: [],
    bounds: { width: 1400, height: 300 },
    columnHeaders: [],
    lanes: [],
  } as ThreadLayout;

  it('counts the stages that start beyond the right edge', () => {
    expect(columnsOffRight(layout, { x: 0, y: 0, zoom: 1 }, 700)).toBe(2); // 870 and 1160
    expect(columnsOffRight(layout, { x: -400, y: 0, zoom: 1 }, 700)).toBe(1); // only 1160
    expect(columnsOffRight(layout, { x: -600, y: 0, zoom: 1 }, 700)).toBe(0);
  });
  it('is zero when everything starts inside the window, or the size is unknown', () => {
    expect(columnsOffRight(layout, { x: 0, y: 0, zoom: 1 }, 1500)).toBe(0);
    expect(columnsOffRight(layout, { x: 0, y: 0, zoom: 1 }, 0)).toBe(0);
  });
  it('accounts for zoom', () => {
    expect(columnsOffRight(layout, { x: 0, y: 0, zoom: 0.5 }, 700)).toBe(0); // 1160*.5 = 580
  });
});
