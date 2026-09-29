import type { LineageNode, ListLineageResponse } from '../client/types';
import { pageSlug } from '../shared/pageId';
import type {
  NodeTarget,
  NodeTone,
  ThreadNode,
  ThreadNodeKind,
  ThreadView,
} from '../shared/protocol';
import { pluralise } from '../shared/text';

/**
 * An immutable folded store of lineage nodes across any number of paged reads.
 * Keyed by node id so lookups and parenting traversals stay O(1).
 */
export interface FoldedLineage {
  rootEventId: string;
  nodes: Map<string, LineageNode>;
}

/**
 * Terminal states can never be downgraded back to 'running' by a partial page read.
 * The gateway folds run nodes from raw event logs, and a page that captured earlier
 * rows without the concluding run-finished event will synthesise a 'running' node.
 */
const TERMINAL_RUN_STATES = new Set(['processed', 'failed', 'dead_letter', 'cancelled', 'planned']);

/**
 * Identifies if a node's parent pointer was assigned by the gateway's fallback heuristic.
 * When the true parent run is absent from a lineage page, the gateway falls back to
 * pointing drafts and changesets at the root event.
 */
function isRootFallback(
  node: LineageNode,
  rootEventId: string,
  associatedRunId?: unknown,
): boolean {
  if (node.type !== 'changeset' && node.type !== 'draft') {
    return false;
  }
  const runId =
    typeof node.run_id === 'string'
      ? node.run_id
      : typeof associatedRunId === 'string'
        ? associatedRunId
        : undefined;

  return Boolean(node.parent && node.parent === rootEventId && runId && runId !== rootEventId);
}

/**
 * Resolves the parent between two versions of the same node across lineage pages.
 * A specific parent (e.g. run id) must never be overwritten by a root fallback,
 * while a specific parent from a later page replaces an earlier root fallback.
 */
function resolveParent(
  existing: LineageNode,
  incoming: LineageNode,
  rootEventId: string,
): string | null {
  const existingIsFallback = isRootFallback(existing, rootEventId, incoming.run_id);
  const incomingIsFallback = isRootFallback(incoming, rootEventId, existing.run_id);

  if (existingIsFallback && !incomingIsFallback) {
    return incoming.parent;
  }
  if (!existingIsFallback && incomingIsFallback) {
    return existing.parent;
  }
  return incoming.parent;
}

/**
 * Preserves terminal run state across pages. A terminal run state is immutable,
 * preventing intermediate 'running' folds from corrupting settled status.
 */
function mergeRunState(existingState?: string | null, incomingState?: string | null): string {
  if (existingState && TERMINAL_RUN_STATES.has(existingState) && incomingState === 'running') {
    return existingState;
  }
  if (incomingState && TERMINAL_RUN_STATES.has(incomingState) && existingState === 'running') {
    return incomingState;
  }
  return incomingState || existingState || 'running';
}

/**
 * Merges any number of list_lineage pages into a unified node store keyed by id.
 * Pure function: operates strictly on domain data with no side-effects or external state.
 */
export function foldLineage(pages: ListLineageResponse[]): FoldedLineage {
  const rootEventId = pages.find((p) => p.root_event_id)?.root_event_id ?? '';
  const nodeStore = new Map<string, LineageNode>();

  for (const page of pages) {
    for (const incoming of page.nodes) {
      const existing = nodeStore.get(incoming.id);
      if (!existing) {
        nodeStore.set(incoming.id, { ...incoming });
        continue;
      }

      // Attributes union across pages; later page values win except for run state and parent
      const merged: LineageNode = { ...existing, ...incoming };

      merged.parent = resolveParent(existing, incoming, rootEventId);

      if (merged.type === 'run') {
        merged.state = mergeRunState(existing.state, incoming.state);
      }

      nodeStore.set(incoming.id, merged);
    }
  }

  return {
    rootEventId,
    nodes: nodeStore,
  };
}

/**
 * Parses timestamps in RFC 3339 format or zoneless UTC space-separated format
 * (e.g. from runner logs) into standard Date objects.
 */
function parseTimestamp(raw?: string | null): Date | null {
  if (!raw) return null;
  const clean = raw.includes(' ') && !raw.includes('Z') ? raw.replace(' ', 'T') + 'Z' : raw;
  const d = new Date(clean);
  return Number.isNaN(d.getTime()) ? null : d;
}

/**
 * Renders time portion as UTC HH:MM:SS to guarantee deterministic, timezone-independent display.
 */
function formatTime(raw?: string | null): string {
  const d = parseTimestamp(raw);
  if (!d) return '';
  const h = String(d.getUTCHours()).padStart(2, '0');
  const m = String(d.getUTCMinutes()).padStart(2, '0');
  const s = String(d.getUTCSeconds()).padStart(2, '0');
  return `${h}:${m}:${s}`;
}

/**
 * Calculates human-readable elapsed duration between two timestamps.
 */
function formatDuration(startedAt?: string | null, finishedAt?: string | null): string {
  const start = parseTimestamp(startedAt);
  const end = parseTimestamp(finishedAt);
  if (!start || !end) return '';
  const diffSec = Math.max(0, Math.round((end.getTime() - start.getTime()) / 1000));
  if (diffSec < 60) return `${diffSec} s`;
  const mins = Math.floor(diffSec / 60);
  const secs = diffSec % 60;
  return secs > 0 ? `${mins} m ${secs} s` : `${mins} m`;
}

/**
 * Shortens ULID/UUID identifiers for display while leaving compact IDs intact.
 */
function shortId(id: string): string {
  return id.length > 8 ? id.slice(0, 8) : id;
}

/**
 * Extracts numeric timestamp for stable chronological ordering.
 */
function getNodeTimestamp(node: LineageNode): number {
  const raw = (node.at ?? node.started_at ?? node.created_at) as string | undefined;
  const parsed = parseTimestamp(raw);
  return parsed ? parsed.getTime() : 0;
}

/**
 * Stable tie-breaker comparing node timestamp ascending, then id lexicographically.
 */
function compareNodes(a: LineageNode, b: LineageNode): number {
  const timeA = getNodeTimestamp(a);
  const timeB = getNodeTimestamp(b);
  if (timeA !== timeB) return timeA - timeB;
  return a.id.localeCompare(b.id);
}

/**
 * Extracts tool call counts from either direct attribute or summary block.
 */
function getToolCallCount(node: LineageNode): number | undefined {
  if (typeof node.tool_calls === 'number') {
    return node.tool_calls;
  }
  const summary = node.tool_call_summary as { count?: number } | undefined;
  if (summary && typeof summary.count === 'number') {
    return summary.count;
  }
  return undefined;
}

/**
 * Translates a folded lineage graph into the protocol view required by the webview.
 * Establishes stable child order, resolves orphaned parents to root, and builds card representations.
 */
export function toThreadView(folded: FoldedLineage): ThreadView {
  const allNodes = Array.from(folded.nodes.values());

  // 1. Resolve view parents. Orphaned nodes whose parent is absent from the node set
  // hang off the root event so the thread graph never drops returned data.
  const viewParentMap = new Map<string, string | null>();
  const childrenMap = new Map<string, string[]>();

  for (const node of allNodes) {
    let parent: string | null = null;
    if (node.id === folded.rootEventId) {
      parent = null;
    } else if (node.parent && folded.nodes.has(node.parent)) {
      parent = node.parent;
    } else {
      parent = folded.rootEventId;
    }
    viewParentMap.set(node.id, parent);

    if (parent) {
      const siblings = childrenMap.get(parent) ?? [];
      siblings.push(node.id);
      childrenMap.set(parent, siblings);
    }
  }

  // 2. Sort children lists in stable chronological order.
  for (const children of childrenMap.values()) {
    children.sort((aId, bId) => {
      const nodeA = folded.nodes.get(aId);
      const nodeB = folded.nodes.get(bId);
      if (!nodeA || !nodeB) return aId.localeCompare(bId);
      return compareNodes(nodeA, nodeB);
    });
  }

  // 3. Transform nodes into ThreadNode protocol objects.
  const threadNodes: ThreadNode[] = allNodes.map((node) => {
    const parent = viewParentMap.get(node.id) ?? null;
    const children = childrenMap.get(node.id) ?? [];
    const state = (node.state as string) ?? null;

    const isRunFailed =
      node.type === 'run' &&
      (state === 'failed' || state === 'dead_letter' || state === 'cancelled');

    let tone: NodeTone = 'neutral';
    if (node.type === 'event') {
      tone = 'event';
    } else if (node.type === 'run') {
      tone = isRunFailed ? 'failed' : 'run';
    } else if (node.type === 'changeset' || node.type === 'draft') {
      tone = 'instance';
    }

    let title = '';
    let subtitle: string | undefined;
    const meta: string[] = [];

    if (node.type === 'event') {
      title = (node.label_skill as string) || '';
      subtitle = (node.title as string) || undefined;
      const time = formatTime(node.at as string);
      const kind = (node.kind as string) || '';
      const line = [time, kind].filter(Boolean).join(' · ');
      if (line) meta.push(line);
    } else if (node.type === 'run') {
      // Runs take their title from the triggering event's skill
      const parentEvent = node.parent ? folded.nodes.get(node.parent) : undefined;
      title =
        (parentEvent?.label_skill as string) ||
        (node.label_skill as string) ||
        (node.skill as string) ||
        'run';
      subtitle = 'run';

      const line1 = [node.harness, node.autonomy].filter(Boolean).join(' · ');
      if (line1) meta.push(line1);

      if (node.started_at && node.finished_at) {
        const dur = formatDuration(node.started_at as string, node.finished_at as string);
        meta.push(
          `${formatTime(node.started_at as string)} → ${formatTime(node.finished_at as string)} · ${dur}`,
        );
      } else if (node.started_at) {
        meta.push(formatTime(node.started_at as string));
      }

      const callCount = getToolCallCount(node);
      if (typeof callCount === 'number') {
        meta.push(pluralise(callCount, 'tool call'));
      }

      if (typeof node.summary === 'string' && node.summary) {
        meta.push(node.summary);
      }
    } else if (node.type === 'changeset') {
      title = `changeset ${shortId(node.id)}`;
      const draftsCount =
        typeof node.drafts === 'number'
          ? node.drafts
          : children.filter((cId) => folded.nodes.get(cId)?.type === 'draft').length;
      meta.push(pluralise(draftsCount, 'draft'));
    } else if (node.type === 'draft') {
      title = pageSlug((node.target_page_id as string) || '');
    }

    const chips = state
      ? [
          {
            text: state,
            tone:
              state === 'failed' || state === 'dead_letter' || state === 'cancelled'
                ? ('failed' as const)
                : tone,
          },
        ]
      : [];

    let target: NodeTarget = { open: 'nothing' };
    if (node.type === 'event') {
      target = { open: 'thread', rootEventId: node.id };
    } else if (node.type === 'run') {
      target = { open: 'run', runId: node.id };
    } else if (node.type === 'changeset') {
      target = { open: 'review', changesetId: node.id };
    } else if (node.type === 'draft') {
      const changesetId = typeof node.changeset_id === 'string' ? node.changeset_id : undefined;
      target = {
        open: 'review',
        draftId: node.id,
        ...(changesetId ? { changesetId } : {}),
      };
    }

    let gate: { drafts: number; changesetId?: string; draftId?: string } | undefined;
    if (node.type === 'changeset' && state === 'open') {
      const draftsCount =
        typeof node.drafts === 'number'
          ? node.drafts
          : children.filter((cId) => folded.nodes.get(cId)?.type === 'draft').length;
      gate = { drafts: draftsCount, changesetId: node.id };
    } else if (node.type === 'draft' && state === 'open' && !node.changeset_id) {
      // Solo draft open gate (present only when the draft does not belong to a changeset)
      const parentNode = parent ? folded.nodes.get(parent) : undefined;
      if (parentNode?.type !== 'changeset') {
        gate = { drafts: 1, draftId: node.id };
      }
    }

    const collapsible = node.type === 'run' || children.length > 0;

    return {
      id: node.id,
      kind: node.type as ThreadNodeKind,
      parent,
      children,
      state,
      tone,
      title,
      subtitle,
      meta,
      chips,
      target,
      gate,
      collapsible,
    };
  });

  // 4. Calculate column headers based on max execution depth.
  let maxDepth = 0;
  for (const node of allNodes) {
    if (typeof node.depth === 'number') {
      maxDepth = Math.max(maxDepth, node.depth);
    }
  }

  const columns = [
    'root event',
    'run · changeset',
    'instances · drafts',
    'cascade · depth 1',
    'outbound · depth 2',
  ];

  for (let d = 3; d <= maxDepth; d++) {
    columns.push(`cascade · depth ${d}`);
  }

  return {
    rootEventId: folded.rootEventId,
    nodes: threadNodes,
    columns,
    loadingMore: false,
  };
}
