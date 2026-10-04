import type { LaidOutNode, ThreadLayout, ThreadNode, ThreadView } from '../shared/protocol';

/**
 * Below this zoom a card drops its body and keeps icon, accent bar, title and state chip, at a size that
 * stays readable. The card text is 11px at 100%, so 85% is where it would fall under about 9.4px (WCAG 1.4.4).
 * It was 70% until the UX review found 8-9px text at 80% zoom.
 */
export const LOW_ZOOM_BELOW = 0.85;

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
 * The node a thread opens on. The first one that waits on a person (main row first, then lanes,
 * left to right). When nothing waits on anyone, the ROOT: the story starts there, and a thread that
 * opened scrolled to its newest node left the person without the beginning. If the root is hidden
 * (collapsed away) or unknown, the first visible card in document order.
 */
export function pickTarget(view: ThreadView, layout: ThreadLayout): string | undefined {
  const ordered = documentOrder(layout);
  if (ordered.length === 0) return undefined;
  const byId = new Map<string, ThreadNode>(view.nodes.map((n) => [n.id, n]));
  const waiting = ordered.find((l) => byId.get(l.id)?.needsYou);
  if (waiting) return waiting.id;
  const root = ordered.find((l) => l.id === view.rootEventId);
  return (root ?? ordered[0])?.id;
}

const clamp = (v: number, lo: number, hi: number): number => Math.min(hi, Math.max(lo, v));

const LEFT_MARGIN = 24;

/**
 * The first viewport. A graph that fits the canvas at 100% opens as it is. A bigger one opens at 100%
 * with the target at the LEFT (with a margin), starting one column earlier when the neighbour and the
 * target both fit, so the card to its left is whole and nothing is cut at the left edge (centring the
 * target cut the first card in half). Clamped so the view never scrolls past the graph. A container
 * height of 0 means "not measured yet" and is not treated as overflow.
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

  let x = 0;
  if (overflowX) {
    const before = layout.nodes
      .filter((n) => !n.hidden && n.x < target.x)
      .reduce<number | undefined>(
        (best, n) => (best === undefined || n.x > best ? n.x : best),
        undefined,
      );
    const fromTarget = target.x - LEFT_MARGIN;
    const fromNeighbour = before !== undefined ? before - LEFT_MARGIN : undefined;
    const fits =
      fromNeighbour !== undefined &&
      target.x + target.width + LEFT_MARGIN - fromNeighbour <= container.width;
    x = clamp(-(fits ? fromNeighbour : fromTarget), container.width - width, 0);
  }
  const cy = target.y + target.height / 2;
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
    const size = view / scaled;
    return { size, pos: clamp(-offset / scaled, 0, 1 - size) };
  };
  return {
    h: axis(bounds.width, container.width, viewport.x),
    v: axis(bounds.height, container.height, viewport.y),
  };
}

/**
 * How many stages (columns) start beyond the right edge of the canvas: the cue that "there is more
 * this way". Columns are the distinct `x` positions of the visible cards.
 */
export function columnsOffRight(
  layout: ThreadLayout,
  viewport: Viewport,
  areaWidth: number,
): number {
  if (areaWidth <= 0) return 0;
  const xs = new Set(layout.nodes.filter((n) => !n.hidden).map((n) => n.x));
  let off = 0;
  for (const x of xs) if (x * viewport.zoom + viewport.x >= areaWidth) off += 1;
  return off;
}
