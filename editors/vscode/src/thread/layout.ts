import type {
  FocusGraph,
  FocusStep,
  LaidOutNode,
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
export const GAP_X = 50;
export const GAP_Y = 24;
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

/**
 * Lays out a ThreadView onto a 2D coordinate space.
 * Nodes stack top-to-bottom within each column, and parents align vertically
 * with their first visible child where column constraints allow, preserving
 * straight horizontal reading paths across steps.
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

  const nextRowPerCol: number[] = [];
  const assignedRow = new Map<string, number>();

  function getVisibleChildren(nodeId: string): ThreadNode[] {
    const parent = nodeMap.get(nodeId);
    if (!parent) return [];
    return parent.children
      .map((id) => nodeMap.get(id))
      .filter((c): c is ThreadNode => Boolean(c && !hiddenMap.get(c.id)));
  }

  function assignRowRecursive(nodeId: string): void {
    if (hiddenMap.get(nodeId)) {
      return;
    }
    const node = nodeMap.get(nodeId);
    if (!node) return;

    const col = colMap.get(node.id) ?? 0;
    while (nextRowPerCol.length <= col) {
      nextRowPerCol.push(0);
    }

    let row = assignedRow.get(node.id);
    if (row === undefined) {
      const visibleChildren = getVisibleChildren(node.id);
      const firstChild = visibleChildren[0];

      if (firstChild) {
        const childCol = colMap.get(firstChild.id) ?? 0;
        while (nextRowPerCol.length <= childCol) {
          nextRowPerCol.push(0);
        }

        if (childCol !== col) {
          // Align parent with first visible child across columns where both columns
          // are currently clear at that vertical offset.
          const sharedRow = Math.max(nextRowPerCol[col] ?? 0, nextRowPerCol[childCol] ?? 0);
          row = sharedRow;
          assignedRow.set(node.id, row);
          assignedRow.set(firstChild.id, row);
          nextRowPerCol[col] = row + 1;
          nextRowPerCol[childCol] = row + 1;
        } else {
          // Same column (e.g. run and changeset): cannot share vertical row, so parent
          // claims next available row and child will stack beneath it.
          const currentRow = nextRowPerCol[col] ?? 0;
          row = currentRow;
          assignedRow.set(node.id, row);
          nextRowPerCol[col] = row + 1;
        }
      } else {
        const currentRow = nextRowPerCol[col] ?? 0;
        row = currentRow;
        assignedRow.set(node.id, row);
        nextRowPerCol[col] = row + 1;
      }
    }

    for (const child of getVisibleChildren(node.id)) {
      assignRowRecursive(child.id);
    }
  }

  // Traverse from root down depth-first in children order.
  const rootNode =
    nodeMap.get(view.rootEventId) ?? orderedNodes.find((n) => n.parent === null) ?? orderedNodes[0];

  if (rootNode) {
    assignRowRecursive(rootNode.id);
  }

  // Cover disconnected or unparented nodes so ACL-pruned trees still layout deterministically.
  for (const node of orderedNodes) {
    if (!assignedRow.has(node.id) && !hiddenMap.get(node.id)) {
      assignRowRecursive(node.id);
    }
  }

  const laidOutNodes: LaidOutNode[] = orderedNodes.map((node) => {
    const isHidden = Boolean(hiddenMap.get(node.id));
    const column = colMap.get(node.id) ?? 0;
    if (isHidden) {
      return {
        id: node.id,
        column,
        x: 0,
        y: 0,
        width: 0,
        height: 0,
        hidden: true,
      };
    }
    const row = assignedRow.get(node.id) ?? 0;
    const x = MARGIN + column * (CARD_WIDTH + GAP_X);
    const y = MARGIN + row * (CARD_HEIGHT + GAP_Y);
    return {
      id: node.id,
      column,
      x,
      y,
      width: CARD_WIDTH,
      height: CARD_HEIGHT,
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
    const path = sameColumn
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

  const columnHeaders = usedColumns.map((col) => ({
    label: view.columns[col]?.trim() || derivedHeader(col),
    x: MARGIN + col * (CARD_WIDTH + GAP_X),
  }));

  return {
    nodes: laidOutNodes,
    wires,
    bounds,
    columnHeaders,
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
