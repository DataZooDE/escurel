import type { LineageNode } from '../client/types';
import { pageSlug } from '../shared/pageId';
import type { InspectorRow, InspectorView, ThreadNode, ThreadView } from '../shared/protocol';
import { formatDateTime, formatDuration } from '../shared/time';

function value(raw: unknown): string | undefined {
  if (raw === null || raw === undefined || raw === '') return undefined;
  if (typeof raw === 'string' || typeof raw === 'number' || typeof raw === 'boolean') {
    return String(raw);
  }
  return undefined;
}

function tone(state: string): InspectorRow['tone'] {
  if (state === 'promoted') return 'ok';
  if (['failed', 'dead_letter', 'cancelled', 'discarded'].includes(state)) return 'error';
  if (state === 'open') return 'warn';
  return undefined;
}

function row(k: string, raw: unknown, state = false): InspectorRow | undefined {
  const v = value(raw);
  if (v === undefined) return undefined;
  const row: InspectorRow = { k, v };
  if (state) row.tone = tone(v);
  return row;
}

function rows(...items: (InspectorRow | undefined)[]): InspectorRow[] {
  return items.filter((item): item is InspectorRow => item !== undefined);
}

function stringAttr(node: LineageNode, key: string): string | undefined {
  return typeof node[key] === 'string' ? (node[key] as string) : undefined;
}

function eventCounts(
  node: ThreadNode,
  byId: Map<string, ThreadNode>,
): { runs: number; events: number } {
  const counts = { runs: 0, events: 0 };
  const seen = new Set<string>([node.id]);
  const pending = [...node.children];
  while (pending.length) {
    const id = pending.pop()!;
    if (seen.has(id)) continue;
    seen.add(id);
    const child = byId.get(id);
    if (!child) continue;
    if (child.kind === 'run') counts.runs++;
    if (child.kind === 'event') counts.events++;
    pending.push(...child.children);
  }
  return counts;
}

function eventDetail(
  node: ThreadNode,
  raw: LineageNode,
  byId: Map<string, ThreadNode>,
): InspectorView {
  const counts = eventCounts(node, byId);
  return {
    title: node.title,
    rows: rows(
      row('label_skill', raw.label_skill),
      row('kind', raw.kind),
      row('at', formatDateTime(raw.at)),
      row('instance_page_id', raw.instance_page_id),
      row('state', raw.state, true),
      row('depth', raw.depth),
    ),
    sideTitle: 'Thread',
    side: rows(row('depth', raw.depth), row('runs', counts.runs), row('events', counts.events)),
  };
}

function runDetail(node: ThreadNode, raw: LineageNode): InspectorView {
  const attempts =
    typeof raw.attempts === 'number' && typeof raw.max_attempts === 'number'
      ? `${raw.attempts}/${raw.max_attempts}`
      : undefined;
  const produced =
    value(raw.produced_instance) && value(raw.produced_version)
      ? `${raw.produced_instance} @ ${raw.produced_version}`
      : undefined;
  const summary = raw.tool_call_summary;
  const failedCalls =
    summary && typeof summary === 'object' && 'failed' in summary ? summary.failed : undefined;
  const body = stringAttr(raw, 'summary');
  return {
    title: node.title,
    rows: rows(
      row('state', raw.state, true),
      row('harness', raw.harness),
      row('model', raw.model),
      row('autonomy', raw.autonomy),
      row('attempts', attempts),
      row('tool calls', raw.tool_calls),
      row('failed calls', failedCalls),
      row('trace_id', raw.trace_id),
      row('produced', produced),
      row('reason', raw.reason),
      row('held', raw.held),
    ),
    ...(body ? { bodyTitle: 'Summary', body } : {}),
    sideTitle: 'Timing',
    side: rows(
      row('started', formatDateTime(raw.started_at)),
      row('finished', formatDateTime(raw.finished_at)),
      row('duration', formatDuration(raw.started_at, raw.finished_at)),
    ),
  };
}

function changesetDetail(
  node: ThreadNode,
  raw: LineageNode,
  byId: Map<string, ThreadNode>,
  rawById: Map<string, LineageNode>,
): InspectorView {
  return {
    title: node.title,
    rows: rows(
      row('state', raw.state, true),
      row('drafts', raw.drafts),
      row('author', raw.author),
      row('run', raw.run_id),
    ),
    sideTitle: 'Drafts',
    side: node.children.flatMap((id) => {
      if (byId.get(id)?.kind !== 'draft') return [];
      const draft = rawById.get(id);
      if (!draft) return [];
      const target = stringAttr(draft, 'target_page_id');
      const item = target ? row(pageSlug(target), draft.state, true) : undefined;
      return item ? [item] : [];
    }),
  };
}

function draftDetail(node: ThreadNode, raw: LineageNode): InspectorView {
  return {
    title: node.title,
    rows: rows(
      row('target', raw.target_page_id),
      row('state', raw.state, true),
      row('author', raw.author),
      row('decided_by', raw.decided_by),
      row('created_at', formatDateTime(raw.created_at)),
    ),
    sideTitle: 'Open page',
    side: rows(row('target_page_id', raw.target_page_id)),
  };
}

/** Join real lineage attributes to display nodes without filling absent gateway fields. */
export function buildInspectors(
  view: ThreadView,
  nodes: LineageNode[],
): Record<string, InspectorView> {
  const byId = new Map(view.nodes.map((node) => [node.id, node]));
  const rawById = new Map(nodes.map((node) => [node.id, node]));
  const details: Record<string, InspectorView> = {};
  for (const node of view.nodes) {
    const raw = rawById.get(node.id);
    if (!raw) continue;
    details[node.id] =
      node.kind === 'event'
        ? eventDetail(node, raw, byId)
        : node.kind === 'run'
          ? runDetail(node, raw)
          : node.kind === 'changeset'
            ? changesetDetail(node, raw, byId, rawById)
            : draftDetail(node, raw);
  }
  return details;
}
