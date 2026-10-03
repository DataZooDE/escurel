import { emphasisOf, needsYouOf } from './nodeStyle';
import type { LineageNode, ListLineageResponse } from '../client/types';
import { pageSlug } from '../shared/pageId';
import type { ThreadNode, ThreadView } from '../shared/protocol';
import { pluralise } from '../shared/text';
import { formatClock, formatDuration, parseGatewayTime } from '../shared/time';

/** Immutable lineage store keyed by id for paging and parent lookup. */
export interface FoldedLineage {
  rootEventId: string;
  nodes: Map<string, LineageNode>;
}

const TERMINAL_RUN_STATES = new Set(['processed', 'failed', 'dead_letter', 'cancelled', 'planned']);

/** A root parent is a fallback only when another copy identifies its real run. */
function isRootFallback(node: LineageNode, rootEventId: string, otherCopyRunId?: unknown): boolean {
  if (node.type !== 'changeset' && node.type !== 'draft') return false;
  const runId =
    typeof node.run_id === 'string'
      ? node.run_id
      : typeof otherCopyRunId === 'string'
        ? otherCopyRunId
        : undefined;
  return Boolean(node.parent && node.parent === rootEventId && runId && runId !== rootEventId);
}

function resolveParent(
  existing: LineageNode,
  incoming: LineageNode,
  rootEventId: string,
): string | null {
  const existingFallback = isRootFallback(existing, rootEventId, incoming.run_id);
  const incomingFallback = isRootFallback(incoming, rootEventId, existing.run_id);
  if (existingFallback && !incomingFallback) return incoming.parent;
  if (!existingFallback && incomingFallback) return existing.parent;
  return incoming.parent;
}

function mergeRunState(existing?: string | null, incoming?: string | null): string | null {
  if (existing && TERMINAL_RUN_STATES.has(existing) && !TERMINAL_RUN_STATES.has(incoming ?? '')) {
    return existing;
  }
  if (incoming && TERMINAL_RUN_STATES.has(incoming)) {
    // Two settled reports can disagree; page order is the only available precedence.
    return incoming;
  }
  return incoming ?? existing ?? null;
}

/** Merge pages by id, retaining real attributes through partial later snapshots. */
export function foldLineage(pages: ListLineageResponse[]): FoldedLineage {
  if (pages.length === 0) return { rootEventId: '', nodes: new Map() };
  const rootEventId = pages.find((page) => page.root_event_id)?.root_event_id ?? '';
  const nodes = new Map<string, LineageNode>();
  for (const page of pages) {
    for (const incoming of page.nodes) {
      const existing = nodes.get(incoming.id);
      if (!existing) {
        nodes.set(incoming.id, { ...incoming });
        continue;
      }
      const nonNullAttributes = Object.fromEntries(
        Object.entries(incoming).filter(([, value]) => value !== null && value !== undefined),
      );
      const merged: LineageNode = { ...existing, ...nonNullAttributes };
      merged.parent = resolveParent(existing, incoming, rootEventId);
      if (merged.type === 'run') {
        // The wire type says string, but partial gateway pages can omit the state.
        Object.assign(merged, { state: mergeRunState(existing.state, incoming.state) });
      }
      nodes.set(incoming.id, merged);
    }
  }
  return { rootEventId, nodes };
}

/** Read string attributes from the gateway's open attribute bag in one place. */
function stringAttr(node: LineageNode | undefined, key: string): string | undefined {
  const value = node?.[key];
  return typeof value === 'string' ? value : undefined;
}

function shortId(id: string): string {
  return /^[0-9A-HJKMNP-TV-Z]{26}$/.test(id) ? id.slice(-6) : id;
}

function timestamp(node: LineageNode): number | undefined {
  return parseGatewayTime(node.at ?? node.started_at ?? node.created_at)?.getTime();
}

function compareIds(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function compareNodes(a: LineageNode, b: LineageNode): number {
  const aTime = timestamp(a);
  const bTime = timestamp(b);
  if (aTime === undefined) return bTime === undefined ? compareIds(a.id, b.id) : 1;
  if (bTime === undefined) return -1;
  return aTime - bTime || compareIds(a.id, b.id);
}

function getToolCallCount(node: LineageNode): number | undefined {
  if (typeof node.tool_calls === 'number') return node.tool_calls;
  const summary = node.tool_call_summary;
  if (summary && typeof summary === 'object' && 'count' in summary) {
    return typeof summary.count === 'number' ? summary.count : undefined;
  }
  return undefined;
}

function resolveViewParents(folded: FoldedLineage): {
  parents: Map<string, string | null>;
  children: Map<string, string[]>;
} {
  const parents = new Map<string, string | null>();
  const children = new Map<string, string[]>();
  for (const node of folded.nodes.values()) {
    const parent =
      node.id === folded.rootEventId
        ? null
        : node.parent && folded.nodes.has(node.parent)
          ? node.parent
          : folded.rootEventId;
    parents.set(node.id, parent);
    if (parent) children.set(parent, [...(children.get(parent) ?? []), node.id]);
  }
  for (const siblings of children.values()) {
    siblings.sort((a, b) => {
      const first = folded.nodes.get(a);
      const second = folded.nodes.get(b);
      return first && second ? compareNodes(first, second) : compareIds(a, b);
    });
  }
  return { parents, children };
}

type CardDetails = Pick<
  ThreadNode,
  'title' | 'subtitle' | 'meta' | 'target' | 'gate' | 'tone' | 'changeset'
>;

function eventCard(node: LineageNode): CardDetails {
  const line = [formatClock(node.at), stringAttr(node, 'kind')].filter(Boolean).join(' · ');
  return {
    title: stringAttr(node, 'label_skill') ?? '',
    subtitle: stringAttr(node, 'title'),
    meta: line ? [line] : [],
    tone: 'event',
    target: { open: 'thread', rootEventId: node.id },
  };
}

function runCard(node: LineageNode, folded: FoldedLineage): CardDetails {
  const parentEvent = node.parent ? folded.nodes.get(node.parent) : undefined;
  const state = stringAttr(node, 'state');
  const failed = state === 'failed' || state === 'dead_letter' || state === 'cancelled';
  const meta: string[] = [];
  const details = [stringAttr(node, 'harness'), stringAttr(node, 'autonomy')]
    .filter(Boolean)
    .join(' · ');
  if (details) meta.push(details);
  if (node.started_at && node.finished_at) {
    meta.push(
      `${formatClock(node.started_at)} → ${formatClock(node.finished_at)} · ${formatDuration(node.started_at, node.finished_at)}`,
    );
  } else if (node.started_at) {
    meta.push(formatClock(node.started_at));
  }
  const count = getToolCallCount(node);
  if (count !== undefined) meta.push(pluralise(count, 'tool call'));
  const summary = stringAttr(node, 'summary');
  if (summary) meta.push(summary);
  return {
    title:
      stringAttr(parentEvent, 'label_skill') ??
      stringAttr(node, 'label_skill') ??
      stringAttr(node, 'skill') ??
      'run',
    subtitle: 'run',
    meta,
    tone: failed ? 'failed' : 'run',
    target: { open: 'run', runId: node.id },
  };
}

function countDrafts(node: LineageNode, children: string[], folded: FoldedLineage): number {
  return typeof node.drafts === 'number'
    ? node.drafts
    : children.filter((id) => folded.nodes.get(id)?.type === 'draft').length;
}

function changesetCard(node: LineageNode, children: string[], folded: FoldedLineage): CardDetails {
  const drafts = countDrafts(node, children, folded);
  const listed = children
    .map((id) => folded.nodes.get(id))
    .filter((c): c is LineageNode => c?.type === 'draft');
  // The gateway sends no time on a changeset; its drafts were written with it.
  const created = listed
    .map((d) => stringAttr(d, 'created_at'))
    .filter((t): t is string => Boolean(t))
    .sort()[0];
  const author = stringAttr(node, 'author');
  return {
    changeset: {
      ...(author ? { author } : {}),
      ...(created ? { at: created } : {}),
      drafts: listed.map((d) => ({
        id: d.id,
        title: pageSlug(stringAttr(d, 'target_page_id') ?? '') || shortId(d.id),
      })),
    },
    title: `changeset ${shortId(node.id)}`,
    meta: [pluralise(drafts, 'draft')],
    tone: 'instance',
    target: { open: 'review', changesetId: node.id },
    gate: node.state === 'open' ? { drafts, changesetId: node.id } : undefined,
  };
}

function draftCard(node: LineageNode, parent: string | null, folded: FoldedLineage): CardDetails {
  const changesetId = stringAttr(node, 'changeset_id');
  const parentNode = parent ? folded.nodes.get(parent) : undefined;
  return {
    title: pageSlug(stringAttr(node, 'target_page_id') ?? ''),
    meta: [],
    tone: 'instance',
    target: { open: 'review', draftId: node.id, ...(changesetId ? { changesetId } : {}) },
    gate:
      node.state === 'open' && !node.changeset_id && parentNode?.type !== 'changeset'
        ? { drafts: 1, draftId: node.id }
        : undefined,
  };
}

function buildNode(
  node: LineageNode,
  parent: string | null,
  children: string[],
  folded: FoldedLineage,
): ThreadNode {
  const details =
    node.type === 'event'
      ? eventCard(node)
      : node.type === 'run'
        ? runCard(node, folded)
        : node.type === 'changeset'
          ? changesetCard(node, children, folded)
          : draftCard(node, parent, folded);
  const state = stringAttr(node, 'state') ?? null;
  const chips = state
    ? [
        {
          text: state,
          tone:
            state === 'failed' || state === 'dead_letter' || state === 'cancelled'
              ? ('failed' as const)
              : details.tone,
        },
      ]
    : [];
  const inChangeset = parent !== null && folded.nodes.get(parent)?.type === 'changeset';
  const kindOf: ThreadNode['kind'] =
    node.type === 'run' || node.type === 'changeset' || node.type === 'draft' ? node.type : 'event';
  const kind = kindOf;
  const needs = needsYouOf(kind, state, inChangeset);
  return {
    id: node.id,
    kind,
    parent,
    children,
    state,
    ...(needs ? { needsYou: needs } : {}),
    emphasis: needs ? 'needs-you' : emphasisOf(kind, state),
    ...details,
    chips,
    collapsible: node.type === 'run' || children.length > 0,
  };
}

function columnsFor(nodes: Iterable<LineageNode>): string[] {
  let maxDepth = 0;
  for (const node of nodes) {
    if (typeof node.depth === 'number') maxDepth = Math.max(maxDepth, node.depth);
  }
  const columns = [
    'root event',
    'run · changeset',
    'instances · drafts',
    'cascade · depth 1',
    'outbound · depth 2',
  ];
  for (let depth = 3; depth <= maxDepth; depth++) columns.push(`cascade · depth ${depth}`);
  return columns;
}

/** Turn a folded graph into connected cards for the webview. */
export function toThreadView(folded: FoldedLineage): ThreadView {
  const { parents, children } = resolveViewParents(folded);
  const nodes = [...folded.nodes.values()].map((node) =>
    buildNode(node, parents.get(node.id) ?? null, children.get(node.id) ?? [], folded),
  );
  if (nodes.length && !folded.nodes.has(folded.rootEventId)) {
    // The layout needs an actual parent card while a paged root has not arrived.
    nodes.unshift({
      id: folded.rootEventId,
      kind: 'event',
      parent: null,
      children: children.get(folded.rootEventId) ?? [],
      state: null,
      tone: 'event',
      title: 'root event (loading)',
      meta: [],
      chips: [],
      target: { open: 'nothing' },
      collapsible: true,
    });
  }
  return {
    rootEventId: folded.rootEventId,
    nodes,
    columns: columnsFor(folded.nodes.values()),
    loadingMore: false,
  };
}
