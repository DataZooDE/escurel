import type { LaidOutNode, ThreadLayout, ThreadNode, ThreadView } from '../shared/protocol';

/** Below this zoom a card drops its text and keeps icon, accent bar and state chip. */
export const LOW_ZOOM_BELOW = 0.7;

export const isLowZoom = (zoom: number): boolean => zoom < LOW_ZOOM_BELOW;

interface Viewport {
  x: number;
  y: number;
  zoom: number;
}
interface Size {
  width: number;
  height: number;
}

/**
 * Document order of the visible cards: the main row first, then each lane below it, and within a
 * lane left to right (then top to bottom). Lanes are bands of `y`, so a card belongs to the last
 * lane that starts at or above it.
 */
function documentOrder(layout: ThreadLayout): LaidOutNode[] {
  const laneStarts = layout.lanes.map((l) => l.y).sort((a, b) => a - b);
  const laneOf = (n: LaidOutNode): number => {
    let lane = 0;
    laneStarts.forEach((y, i) => {
      if (n.y + n.height / 2 >= y) lane = i;
    });
    return lane;
  };
  return layout.nodes
    .filter((n) => !n.hidden)
    .map((n) => ({ n, lane: laneOf(n) }))
    .sort((a, b) => a.lane - b.lane || a.n.x - b.n.x || a.n.y - b.n.y)
    .map((e) => e.n);
}

/**
 * The node a thread opens on. The first one that waits on a person; else the newest unfinished
 * node (ids are ULIDs, so the greatest id is the newest); else, when everything is finished, the
 * last node of the main row.
 */
export function pickTarget(view: ThreadView, layout: ThreadLayout): string | undefined {
  const ordered = documentOrder(layout);
  if (ordered.length === 0) return undefined;
  const byId = new Map<string, ThreadNode>(view.nodes.map((n) => [n.id, n]));
  const waiting = ordered.find((l) => byId.get(l.id)?.needsYou);
  if (waiting) return waiting.id;

  const active = ordered.filter((l) => {
    const node = byId.get(l.id);
    return node !== undefined && node.emphasis !== 'compact';
  });
  if (active.length > 0) {
    return active.reduce((best, cur) => (cur.id > best.id ? cur : best)).id;
  }

  const laneStarts = layout.lanes.map((l) => l.y).sort((a, b) => a - b);
  const secondLaneY = laneStarts[1] ?? Infinity;
  const mainRow = ordered.filter((l) => l.y + l.height / 2 < secondLaneY);
  return (mainRow[mainRow.length - 1] ?? ordered[ordered.length - 1])?.id;
}

const clamp = (v: number, lo: number, hi: number): number => Math.min(hi, Math.max(lo, v));

/**
 * The first viewport. A graph that fits the canvas at 100% opens as it is. A bigger one opens at 100%
 * with the target centred, clamped so the view never scrolls past the graph. A container height of 0
 * means "not measured yet" and is not treated as overflow.
 */
export function firstViewport(
  layout: ThreadLayout,
  targetId: string | undefined,
  container: Size,
): Viewport {
  const { width, height } = layout.bounds;
  const overflowX = width > container.width;
  const overflowY = container.height > 0 && height > container.height;
  const target = targetId ? layout.nodes.find((n) => n.id === targetId) : undefined;
  if (!target || (!overflowX && !overflowY)) return { x: 0, y: 0, zoom: 1 };

  const cx = target.x + target.width / 2;
  const cy = target.y + target.height / 2;
  const x = overflowX ? clamp(container.width / 2 - cx, container.width - width, 0) : 0;
  const y = overflowY ? clamp(container.height / 2 - cy, container.height - height, 0) : 0;
  return { x, y, zoom: 1 };
}

export interface Thumb {
  /** Share of the graph that is visible (0..1). */
  size: number;
  /** Where the visible part starts, as a share of the graph (0..1). */
  pos: number;
}

/** Scrollbar thumbs for the axes on which the graph is bigger than the canvas. */
export function scrollMetrics(
  viewport: Viewport,
  bounds: Size,
  container: Size,
): { h: Thumb | undefined; v: Thumb | undefined } {
  const axis = (extent: number, view: number, offset: number): Thumb | undefined => {
    const scaled = extent * viewport.zoom;
    if (view <= 0 || scaled <= view) return undefined;
    return { size: view / scaled, pos: clamp(-offset / scaled, 0, 1) };
  };
  return {
    h: axis(bounds.width, container.width, viewport.x),
    v: axis(bounds.height, container.height, viewport.y),
  };
}
