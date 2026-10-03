import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { ListLineageResponse } from '../../src/client/types';
import { foldLineage, toThreadView } from '../../src/thread/threadModel';
import type { ThreadNode, ThreadView } from '../../src/shared/protocol';
import {
  CARD_HEIGHT,
  COMPACT_HEIGHT,
  GAP_Y,
  LANE_GAP,
  layoutThread,
} from '../../src/thread/layout';

type Emphasis = 'compact' | 'normal';

function node(
  id: string,
  kind: ThreadNode['kind'],
  parent: string | null,
  children: string[],
  emphasis: Emphasis = 'normal',
): ThreadNode {
  return {
    id,
    kind,
    parent,
    children,
    state: null,
    tone: 'neutral',
    title: id,
    meta: [],
    chips: [],
    target: { open: 'nothing' },
    collapsible: false,
    emphasis,
  };
}

function view(root: string, nodes: ThreadNode[]): ThreadView {
  return {
    rootEventId: root,
    nodes,
    columns: [
      'root event',
      'run · changeset',
      'instances · drafts',
      'cascade · depth 1',
      'outbound',
    ],
    loadingMore: false,
  };
}

const at = (layout: ReturnType<typeof layoutThread>, id: string) => {
  const found = layout.nodes.find((n) => n.id === id);
  if (!found) throw new Error(`no node ${id}`);
  return found;
};

// Cards of different heights share one grid. The row index used to be multiplied by one fixed card
// height, so a small card either wasted a full row or overlapped its neighbour.
describe('card heights', () => {
  it('lays a finished node out as a small card and an active one at full height', () => {
    const layout = layoutThread(
      view('e', [node('e', 'event', null, ['r'], 'compact'), node('r', 'run', 'e', [], 'normal')]),
      new Set(),
    );
    expect(at(layout, 'e').height).toBe(COMPACT_HEIGHT);
    expect(at(layout, 'r').height).toBe(CARD_HEIGHT);
    expect(COMPACT_HEIGHT < CARD_HEIGHT / 2 + 10).toBe(true);
  });

  it('stacks cards of mixed height in one column without overlap and with the standard gap', () => {
    const layout = layoutThread(
      view('e', [
        node('e', 'event', null, ['r'], 'compact'),
        node('r', 'run', 'e', ['c'], 'compact'),
        node('c', 'changeset', 'r', ['d1', 'd2', 'd3'], 'normal'),
        node('d1', 'draft', 'c', [], 'compact'),
        node('d2', 'draft', 'c', [], 'normal'),
        node('d3', 'draft', 'c', [], 'compact'),
      ]),
      new Set(),
    );
    const column = ['d1', 'd2', 'd3'].map((id) => at(layout, id)).sort((a, b) => a.y - b.y);
    for (let i = 1; i < column.length; i += 1) {
      expect(column[i]!.y - (column[i - 1]!.y + column[i - 1]!.height)).toBe(GAP_Y);
    }
  });
});

// A follow-on chain gets its own row, so branches stop crossing each other.
describe('lane rows per cascade branch', () => {
  const branching = view('e0', [
    node('e0', 'event', null, ['r1']),
    node('r1', 'run', 'e0', ['x1', 'x2']),
    node('x1', 'event', 'r1', ['r2']),
    node('r2', 'run', 'x1', []),
    node('x2', 'event', 'r1', ['r3']),
    node('r3', 'run', 'x2', []),
  ]);

  it('keeps the main chain in the first row and puts the second follow-on in a row below', () => {
    const layout = layoutThread(branching, new Set());
    expect(at(layout, 'x1').y).toBe(at(layout, 'r1').y);
    expect(at(layout, 'r2').y).toBe(at(layout, 'x1').y);
    const mainBottom = Math.max(
      ...['e0', 'r1', 'x1', 'r2'].map((id) => at(layout, id).y + at(layout, id).height),
    );
    expect(at(layout, 'x2').y >= mainBottom + LANE_GAP).toBe(true);
    expect(at(layout, 'r3').y).toBe(at(layout, 'x2').y);
  });

  it('reports the lanes with their extent', () => {
    const layout = layoutThread(branching, new Set());
    expect(layout.lanes.map((l) => l.index)).toEqual([0, 1]);
    expect(layout.lanes[1]!.y > layout.lanes[0]!.y + layout.lanes[0]!.height).toBe(true);
    for (const id of ['x2', 'r3']) {
      expect(at(layout, id).y >= layout.lanes[1]!.y).toBe(true);
    }
  });

  it('has one lane for a thread with a single chain', () => {
    const layout = layoutThread(
      view('e0', [node('e0', 'event', null, ['r1']), node('r1', 'run', 'e0', [])]),
      new Set(),
    );
    expect(layout.lanes).toHaveLength(1);
  });

  it('drops a lane whose branch is collapsed away', () => {
    const layout = layoutThread(branching, new Set(['r1']));
    expect(layout.lanes).toHaveLength(1);
  });
});

// Connectors must not run through cards they do not connect.
function samplePath(path: string): { x: number; y: number }[] {
  const n = path.match(/-?\d+(\.\d+)?/g)!.map(Number);
  if (path.includes('C')) {
    const [x0, y0, x1, y1, x2, y2, x3, y3] = n as [
      number,
      number,
      number,
      number,
      number,
      number,
      number,
      number,
    ];
    return Array.from({ length: 41 }, (_, i) => {
      const t = i / 40;
      const m = 1 - t;
      return {
        x: m ** 3 * x0 + 3 * m * m * t * x1 + 3 * m * t * t * x2 + t ** 3 * x3,
        y: m ** 3 * y0 + 3 * m * m * t * y1 + 3 * m * t * t * y2 + t ** 3 * y3,
      };
    });
  }
  // A polyline: M x y L x y L ...
  const points: { x: number; y: number }[] = [];
  for (let i = 0; i + 1 < n.length; i += 2) points.push({ x: n[i]!, y: n[i + 1]! });
  const out: { x: number; y: number }[] = [];
  for (let i = 1; i < points.length; i += 1) {
    for (let k = 0; k <= 20; k += 1) {
      const a2 = points[i - 1]!;
      const b2 = points[i]!;
      out.push({ x: a2.x + ((b2.x - a2.x) * k) / 20, y: a2.y + ((b2.y - a2.y) * k) / 20 });
    }
  }
  return out;
}

describe('wires', () => {
  it('never pass through a card that is not one of their ends', () => {
    const layout = layoutThread(branching(), new Set());
    for (const wire of layout.wires) {
      for (const p of samplePath(wire.path)) {
        for (const card of layout.nodes) {
          if (card.hidden || card.id === wire.from || card.id === wire.to) continue;
          const inside =
            p.x > card.x + 1 &&
            p.x < card.x + card.width - 1 &&
            p.y > card.y + 1 &&
            p.y < card.y + card.height - 1;
          expect(inside, `${wire.from}→${wire.to} crosses ${card.id}`).toBe(false);
        }
      }
    }
  });
});

function branching(): ThreadView {
  return view('e0', [
    node('e0', 'event', null, ['r1'], 'compact'),
    node('r1', 'run', 'e0', ['c1', 'x1', 'x2'], 'compact'),
    node('c1', 'changeset', 'r1', ['d1', 'd2'], 'compact'),
    node('d1', 'draft', 'c1', [], 'compact'),
    node('d2', 'draft', 'c1', [], 'compact'),
    node('x1', 'event', 'r1', ['r2'], 'normal'),
    node('r2', 'run', 'x1', [], 'normal'),
    node('x2', 'event', 'r1', ['r3'], 'normal'),
    node('r3', 'run', 'x2', [], 'normal'),
  ]);
}

// The recordings are real gateway output: whatever shape they have, the layout must stay clean.
describe.each([
  'lineage-cascade.json',
  'lineage-event-run-changeset-draft.json',
  'lineage-paged-full.json',
])('recorded %s', (file) => {
  const fixture = JSON.parse(
    readFileSync(join(__dirname, 'fixtures', 'lineage', file), 'utf8'),
  ) as ListLineageResponse;
  const layout = layoutThread(toThreadView(foldLineage([fixture])), new Set());
  const cards = layout.nodes.filter((n) => !n.hidden);

  it('never lets two cards overlap', () => {
    for (const a of cards) {
      for (const b of cards) {
        if (a.id >= b.id) continue;
        const apart =
          a.x + a.width <= b.x ||
          b.x + b.width <= a.x ||
          a.y + a.height <= b.y ||
          b.y + b.height <= a.y;
        expect(apart, `${a.id} overlaps ${b.id}`).toBe(true);
      }
    }
  });

  it('never routes a wire through a card it does not connect', () => {
    for (const wire of layout.wires) {
      for (const p of samplePath(wire.path)) {
        for (const card of cards) {
          if (card.id === wire.from || card.id === wire.to) continue;
          const inside =
            p.x > card.x + 1 &&
            p.x < card.x + card.width - 1 &&
            p.y > card.y + 1 &&
            p.y < card.y + card.height - 1;
          expect(inside, `${wire.from}→${wire.to} crosses ${card.id}`).toBe(false);
        }
      }
    }
  });
});
