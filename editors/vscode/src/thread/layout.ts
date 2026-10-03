import type {
  FocusGraph,
  FocusStep,
  LaidOutNode,
  Lane,
  ThreadLayout,
  ThreadNode,
  ThreadView,
  Wire,
} from '../shared/protocol';

export const CARD_WIDTH = 240;
// Tall enough for the most a card carries: title, subtitle, four meta lines and a row of
// chips. At 80 the run card — the fullest — clipped its state chip and its summary, which
// only a screenshot showed.
// Tall enough for a title, a subtitle, four meta lines and the footer at the readable 11px text size.
export const CARD_HEIGHT = 150;
/** A finished node: title and state on two lines, nothing else. */
export const COMPACT_HEIGHT = 58;
/** A live card with nothing but its title, type and state (no meta lines): no empty box. */
export const SHORT_HEIGHT = 84;
/** A node that waits on a person: more room for the badge, the reason and the actions. */
export const NEEDS_YOU_HEIGHT = 176;
/** One listed draft on an open changeset's card, and how many the card lists before "+N more". */
export const DRAFT_ROW_HEIGHT = 22;
export const MAX_LISTED_DRAFTS = 4;
export const GAP_X = 50;
export const GAP_Y = 24;
/** Space between two lanes (cascade branches), over and above a card gap. */
export const LANE_GAP = 40;
export const MARGIN = 32;
// Same-column and backward wires need a minimum bend to remain visible beside card edges.
const MIN_BEZIER_CURVATURE = 20;

/**
 * A node is hidden only when an ancestor is collapsed, never because it is
 * collapsed itself — collapsing a branch conceals its downstream consequences
 * while keeping the gate or run visible so the user can toggle it back.
 */
function isAncestorCollapsed(
  nodeId: string,
  collapsed: ReadonlySet<string>,
  nodeMap: Map<string, ThreadNode>,
): boolean {
  let current = nodeMap.get(nodeId);
  while (current?.parent) {
    if (collapsed.has(current.parent)) {
      return true;
    }
    current = nodeMap.get(current.parent);
  }
  return false;
}

/**
 * Assigns columns deterministically based on tree alternation and role.
 * Changesets share their run's column so they stack vertically as an inline
 * gate ("run · changeset"), while cascade hops skip one column past the run
 * to land to the right of drafts produced by that same run.
 *
 * Resolves recursively so node ordering in the array cannot cause a child
 * to read an uncomputed parent column.
 */
function getNodeColumn(
  nodeId: string,
  rootId: string,
  nodeMap: Map<string, ThreadNode>,
  colMap: Map<string, number>,
  visiting = new Set<string>(),
): number {
  const cached = colMap.get(nodeId);
  if (cached !== undefined) {
    return cached;
  }

  const node = nodeMap.get(nodeId);
  if (!node || !node.parent || node.id === rootId || visiting.has(nodeId)) {
    colMap.set(nodeId, 0);
    return 0;
  }

  visiting.add(nodeId);
  const parentCol = getNodeColumn(node.parent, rootId, nodeMap, colMap, visiting);
  visiting.delete(nodeId);

  let col: number;
  if (node.kind === 'run') {
    col = parentCol + 1;
  } else if (node.kind === 'changeset') {
    // Changesets visually stack underneath their run rather than opening a new
    // column, mirroring the review gate mock.
    col = parentCol;
  } else if (node.kind === 'draft') {
    col = parentCol + 1;
  } else if (node.kind === 'event') {
    const parent = nodeMap.get(node.parent);
    // A run's hop clears its drafts; other events advance from their direct parent.
    col = parentCol + (parent?.kind === 'run' ? 2 : 1);
  } else {
    col = parentCol + 1;
  }

  colMap.set(nodeId, col);
  return col;
}

/**
 * Computes cubic Bézier control points between parent's right edge and child's
 * left edge. Using horizontal tangents at both ends keeps wire departures and
 * arrivals perpendicular to card borders.
 */
function cubicBezierPath(x1: number, y1: number, x2: number, y2: number): string {
  const dx = x2 - x1;
  const curvature = dx > 0 ? dx / 2 : Math.max(Math.abs(dx) / 2, MIN_BEZIER_CURVATURE);
  const cx1 = x1 + curvature;
  const cx2 = x2 - (dx > 0 ? curvature : -curvature);
  return `M ${x1} ${y1} C ${cx1} ${y1}, ${cx2} ${y2}, ${x2} ${y2}`;
}

/**
 * A wire that skips columns runs out into the gap beside its parent, along the gap to the child's
 * row, then across the (kept clear) corridor to the child. A curve would bend through the pages that
 * sit in the columns between.
 */
function corridorPath(
  parent: { x: number; y: number; width: number; height: number },
  child: { x: number; y: number; height: number },
): string {
  const x1 = parent.x + parent.width;
  const y1 = parent.y + parent.height / 2;
  const x2 = child.x;
  const y2 = child.y + child.height / 2;
  if (Math.abs(y1 - y2) < 1) return `M ${x1} ${y1} L ${x2} ${y2}`;
  const xa = x1 + GAP_X / 2;
  return `M ${x1} ${y1} L ${xa} ${y1} L ${xa} ${y2} L ${x2} ${y2}`;
}

/**
 * A wire into another lane runs through space that holds no card: out of the parent's right edge
 * into the gap beside its column, down into the gap between the lanes, along it, then into the
 * gap before the child's column and across to the child. A straight or curved wire would cut
 * through the cards of the lane in between.
 */
function branchPath(
  parent: { x: number; y: number; width: number; height: number },
  child: { x: number; y: number; height: number },
  laneGapY: number,
): string {
  const x1 = parent.x + parent.width;
  const y1 = parent.y + parent.height / 2;
  const x2 = child.x;
  const y2 = child.y + child.height / 2;
  const xa = x1 + GAP_X / 2;
  const xb = x2 - GAP_X / 2;
  return `M ${x1} ${y1} L ${xa} ${y1} L ${xa} ${laneGapY} L ${xb} ${laneGapY} L ${xb} ${y2} L ${x2} ${y2}`;
}

/**
 * Promoted writes and processed cascade events receive distinctive wire styles
 * so the eye immediately tracks approved drafts and executed outbound hops
 * without inspecting individual card badges.
 */
function wireStyle(child: ThreadNode): Wire['style'] {
  if (child.state === 'promoted') {
    return 'promoted';
  }
  if (child.kind === 'event' && child.parent !== null && child.state === 'processed') {
    return 'sent';
  }
  return 'solid';
}

/** The room a node takes: a finished node is a small card. Heights differ, so y is in pixels. */
export function heightFor(node: ThreadNode): number {
  if (node.emphasis === 'compact') return COMPACT_HEIGHT;
  if (node.emphasis === 'needs-you') {
    const listed = node.changeset?.drafts.length ?? 0;
    // Up to MAX_LISTED_DRAFTS rows; more than that is one "+N more" row.
    const rows = listed <= MAX_LISTED_DRAFTS ? listed : MAX_LISTED_DRAFTS + 1;
    return NEEDS_YOU_HEIGHT + rows * DRAFT_ROW_HEIGHT;
  }
  return node.meta.length === 0 ? SHORT_HEIGHT : CARD_HEIGHT;
}

/**
 * Lane of every node. The main chain is lane 0. A run's FIRST follow-on event continues its lane
 * (the chain keeps reading left to right); every further follow-on event of that run starts a lane
 * of its own, below, so branches stop crossing each other. Everything downstream of a node stays
 * in its lane.
 */
function assignLanes(
  orderedNodes: ThreadNode[],
  nodeMap: Map<string, ThreadNode>,
): { laneOf: Map<string, number>; laneCount: number; starters: Map<number, string> } {
  const laneOf = new Map<string, number>();
  const starters = new Map<number, string>();
  let laneCount = 1;
  for (const node of orderedNodes) {
    const parent = node.parent ? nodeMap.get(node.parent) : undefined;
    if (!parent) {
      laneOf.set(node.id, 0);
      continue;
    }
    const parentLane = laneOf.get(parent.id) ?? 0;
    const startsBranch =
      node.kind === 'event' &&
      parent.kind === 'run' &&
      parent.children.filter((id) => nodeMap.get(id)?.kind === 'event')[0] !== node.id;
    if (startsBranch) {
      laneOf.set(node.id, laneCount);
      starters.set(laneCount, node.id);
      laneCount += 1;
    } else {
      laneOf.set(node.id, parentLane);
    }
  }
  return { laneOf, laneCount, starters };
}

/**
 * Lays out a ThreadView onto a 2D coordinate space.
 * Columns are stages; lanes are cascade branches, stacked top to bottom. Within a lane, nodes stack
 * top to bottom in each column, and a parent aligns with its first visible child where columns
 * allow, preserving straight horizontal reading paths across steps.
 */
export function layoutThread(view: ThreadView, collapsed: ReadonlySet<string>): ThreadLayout {
  const nodeMap = new Map<string, ThreadNode>(view.nodes.map((n) => [n.id, n]));
  const orderedNodes: ThreadNode[] = [];
  const visited = new Set<string>();

  function visit(nodeId: string): void {
    if (visited.has(nodeId)) return;
    const node = nodeMap.get(nodeId);
    if (!node) return;
    visited.add(nodeId);
    orderedNodes.push(node);
    for (const childId of node.children) visit(childId);
  }

  visit(view.rootEventId);
  // Orphans can appear in ACL-pruned views; ID order keeps their placement stable.
  for (const node of [...view.nodes].sort((a, b) => a.id.localeCompare(b.id))) {
    visit(node.id);
  }
  const colMap = new Map<string, number>();

  for (const node of orderedNodes) {
    getNodeColumn(node.id, view.rootEventId, nodeMap, colMap);
  }

  // Determine hidden state up-front: hidden cards take zero space in the
  // grid flow so visible siblings collapse upward into freed space.
  const hiddenMap = new Map<string, boolean>();
  for (const node of orderedNodes) {
    hiddenMap.set(node.id, isAncestorCollapsed(node.id, collapsed, nodeMap));
  }

  const { laneOf, laneCount, starters } = assignLanes(orderedNodes, nodeMap);
  const cursor: number[] = []; // next free y per column, within the lane being placed
  const placedY = new Map<string, number>();
  const lanes: Lane[] = [];
  let laneTop = MARGIN;

  const colOf = (id: string) => colMap.get(id) ?? 0;
  const heightOf = (id: string) => {
    const n = nodeMap.get(id);
    return n ? heightFor(n) : CARD_HEIGHT;
  };
  const cursorAt = (col: number) => cursor[col] ?? laneTop;

  function laneChildren(nodeId: string, lane: number): ThreadNode[] {
    const parent = nodeMap.get(nodeId);
    if (!parent) return [];
    return parent.children
      .map((id) => nodeMap.get(id))
      .filter((c): c is ThreadNode =>
        Boolean(c && !hiddenMap.get(c.id) && laneOf.get(c.id) === lane),
      );
  }

  function place(nodeId: string, lane: number): void {
    if (hiddenMap.get(nodeId)) return;
    const node = nodeMap.get(nodeId);
    if (!node) return;
    const col = colOf(node.id);
    const parentPlaced = node.parent ? nodeMap.get(node.parent) : undefined;
    const parentCol = parentPlaced ? colOf(parentPlaced.id) : col;
    if (
      !placedY.has(node.id) &&
      parentPlaced &&
      placedY.has(parentPlaced.id) &&
      laneOf.get(parentPlaced.id) === lane &&
      Math.abs(col - parentCol) >= 2
    ) {
      // A child that skips columns (a follow-on event, past the pages of the changeset): its wire
      // runs along a corridor through the columns between, so it is placed below everything those
      // columns already hold, and they are kept clear beside it.
      const lo = Math.min(col, parentCol);
      const hi = Math.max(col, parentCol);
      // The parent's own column is already past the parent; only the columns it passes and the
      // child's own column can hold something in the way.
      let y = placedY.get(parentPlaced.id) ?? laneTop;
      for (let c = lo + 1; c <= hi; c += 1) y = Math.max(y, cursorAt(c));
      placedY.set(node.id, y);
      const band = y + heightOf(node.id) + GAP_Y;
      for (let c = lo + 1; c < hi; c += 1) cursor[c] = Math.max(cursorAt(c), band);
      cursor[col] = band;
    }
    if (!placedY.has(node.id)) {
      const first = laneChildren(node.id, lane)[0];
      const childCol = first ? colOf(first.id) : col;
      if (first && childCol !== col) {
        // Align the parent with its first child across columns where both are clear at that
        // offset. Every column BETWEEN them is part of the corridor the wire runs through, so it
        // counts too: pages stacked there from an earlier run reach further down than either end,
        // and the wire cut straight through them.
        const lo = Math.min(col, childCol);
        const hi = Math.max(col, childCol);
        let y = 0;
        for (let c = lo; c <= hi; c += 1) y = Math.max(y, cursorAt(c));
        placedY.set(node.id, y);
        placedY.set(first.id, y);
        const band = y + Math.max(heightOf(node.id), heightOf(first.id)) + GAP_Y;
        for (let c = lo + 1; c < hi; c += 1) cursor[c] = Math.max(cursorAt(c), band);
        cursor[col] = y + heightOf(node.id) + GAP_Y;
        cursor[childCol] = y + heightOf(first.id) + GAP_Y;
      } else {
        // Same column (a changeset under its run) or a leaf: next free slot.
        const y = cursorAt(col);
        placedY.set(node.id, y);
        cursor[col] = y + heightOf(node.id) + GAP_Y;
      }
    }
    for (const child of laneChildren(node.id, lane)) place(child.id, lane);
  }

  const rootNode =
    nodeMap.get(view.rootEventId) ?? orderedNodes.find((n) => n.parent === null) ?? orderedNodes[0];

  for (let lane = 0; lane < laneCount; lane += 1) {
    cursor.length = 0;
    const start = lane === 0 ? rootNode?.id : starters.get(lane);
    if (start) place(start, lane);
    if (lane === 0) {
      // Disconnected or unparented nodes so ACL-pruned trees still lay out deterministically.
      for (const node of orderedNodes) {
        if (laneOf.get(node.id) === 0 && !placedY.has(node.id) && !hiddenMap.get(node.id)) {
          place(node.id, 0);
        }
      }
    }
    const members = orderedNodes.filter((n) => laneOf.get(n.id) === lane && placedY.has(n.id));
    if (members.length === 0) continue;
    const bottom = Math.max(...members.map((n) => (placedY.get(n.id) ?? 0) + heightOf(n.id)));
    const starter = starters.get(lane);
    lanes.push({
      index: lane,
      y: laneTop,
      height: bottom - laneTop,
      ...(starter ? { title: nodeMap.get(starter)?.title ?? '' } : {}),
    });
    laneTop = bottom + LANE_GAP;
  }

  const laidOutNodes: LaidOutNode[] = orderedNodes.map((node) => {
    const isHidden = Boolean(hiddenMap.get(node.id)) || !placedY.has(node.id);
    const column = colMap.get(node.id) ?? 0;
    if (isHidden) {
      return { id: node.id, column, x: 0, y: 0, width: 0, height: 0, hidden: true };
    }
    return {
      id: node.id,
      column,
      x: MARGIN + column * (CARD_WIDTH + GAP_X),
      y: placedY.get(node.id) ?? MARGIN,
      width: CARD_WIDTH,
      height: heightFor(node),
      hidden: false,
    };
  });

  const laidOutMap = new Map(laidOutNodes.map((n) => [n.id, n]));

  // Connect visible parent-child pairs. Wires are omitted when either end
  // is hidden by collapse to avoid orphan lines pointing into empty space.
  const wires: Wire[] = [];
  for (const child of orderedNodes) {
    if (hiddenMap.get(child.id) || !child.parent || hiddenMap.get(child.parent)) {
      continue;
    }
    const parentLayout = laidOutMap.get(child.parent);
    const childLayout = laidOutMap.get(child.id);
    if (!parentLayout || !childLayout || parentLayout.hidden || childLayout.hidden) {
      continue;
    }

    // A child in its parent's own column — a changeset under its run — hangs below it, so the
    // wire goes bottom to top. Right edge to left edge would leave the parent's right side,
    // loop out and double back across the card.
    const sameColumn = parentLayout.column === childLayout.column;
    const childLane = lanes.find((lane) => lane.index === laneOf.get(child.id));
    const crossesLanes = laneOf.get(child.id) !== laneOf.get(child.parent) && childLane;
    const skipsColumns = Math.abs(childLayout.column - parentLayout.column) >= 2;
    const path = crossesLanes
      ? branchPath(parentLayout, childLayout, childLane.y - LANE_GAP / 2)
      : skipsColumns
        ? corridorPath(parentLayout, childLayout)
        : sameColumn
          ? `M ${parentLayout.x + parentLayout.width / 2} ${parentLayout.y + parentLayout.height} L ${childLayout.x + childLayout.width / 2} ${childLayout.y}`
          : cubicBezierPath(
              parentLayout.x + parentLayout.width,
              parentLayout.y + parentLayout.height / 2,
              childLayout.x,
              childLayout.y + childLayout.height / 2,
            );

    wires.push({ from: child.parent, to: child.id, path, style: wireStyle(child) });
  }

  const visibleCards = laidOutNodes.filter((n) => !n.hidden);
  const bounds =
    visibleCards.length === 0
      ? { width: 0, height: 0 }
      : {
          width: Math.max(...visibleCards.map((n) => n.x + n.width)) + MARGIN,
          height: Math.max(...visibleCards.map((n) => n.y + n.height)) + MARGIN,
        };

  const usedColumns = Array.from(new Set(visibleCards.map((n) => n.column))).sort((a, b) => a - b);

  function derivedHeader(col: number): string {
    const occupant = orderedNodes.find(
      (node) => !hiddenMap.get(node.id) && colMap.get(node.id) === col,
    );
    if (!occupant) return `depth ${col}`;
    let depth = 0;
    let ancestor: ThreadNode | undefined = occupant;
    while (ancestor) {
      if (ancestor.kind === 'run') depth += 1;
      ancestor = ancestor.parent ? nodeMap.get(ancestor.parent) : undefined;
    }
    const role =
      occupant.kind === 'event'
        ? 'cascade'
        : occupant.kind === 'draft'
          ? 'drafts'
          : 'run · changeset';
    return `${role} · depth ${depth}`;
  }

  // The mock's five labels fit the standard shape; beyond them a fixed list drifts (it put
  // "cascade · depth 3" over a column of pages), so deeper columns are named from their occupants.
  const MOCK_COLUMNS = 5;
  const columnHeaders = usedColumns.map((col) => ({
    label: (col < MOCK_COLUMNS && view.columns[col]?.trim()) || derivedHeader(col),
    x: MARGIN + col * (CARD_WIDTH + GAP_X),
  }));

  return {
    nodes: laidOutNodes,
    wires,
    bounds,
    columnHeaders,
    lanes,
  };
}

/**
 * Builds the directional keyboard focus graph for the thread canvas.
 * Hidden nodes are completely omitted from steps and targets so arrow-key
 * navigation cannot strand focus inside an invisible subtree.
 */
export function focusGraph(view: ThreadView, layout: ThreadLayout): FocusGraph {
  const visibleNodes = layout.nodes.filter((n) => !n.hidden);
  const visibleIds = new Set(visibleNodes.map((n) => n.id));
  const viewNodeMap = new Map(view.nodes.map((n) => [n.id, n]));

  const first = visibleIds.has(view.rootEventId)
    ? view.rootEventId
    : (visibleNodes[0]?.id ?? view.rootEventId);

  const steps: Record<string, FocusStep> = {};

  for (const node of visibleNodes) {
    const vNode = viewNodeMap.get(node.id);
    const step: FocusStep = {};

    // Forward moves to the first visible child along the execution lineage.
    if (vNode) {
      for (const childId of vNode.children) {
        if (visibleIds.has(childId)) {
          step.next = childId;
          break;
        }
      }
    }

    // Back follows the causation chain toward the root event.
    if (vNode?.parent && visibleIds.has(vNode.parent)) {
      step.back = vNode.parent;
    }

    // Up and down navigate siblings within the same column by physical y coordinate.
    let nearestAbove: LaidOutNode | undefined;
    let nearestBelow: LaidOutNode | undefined;

    for (const other of visibleNodes) {
      if (other.id === node.id || other.column !== node.column) {
        continue;
      }
      if (other.y < node.y) {
        if (!nearestAbove || other.y > nearestAbove.y) {
          nearestAbove = other;
        }
      } else if (other.y > node.y) {
        if (!nearestBelow || other.y < nearestBelow.y) {
          nearestBelow = other;
        }
      }
    }

    if (nearestAbove) {
      step.up = nearestAbove.id;
    }
    if (nearestBelow) {
      step.down = nearestBelow.id;
    }

    steps[node.id] = step;
  }

  return { first, steps };
}
