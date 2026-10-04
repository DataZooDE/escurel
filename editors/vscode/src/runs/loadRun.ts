import type { EscurelClient, Event, LineageNode } from '../client';
import type { RunView } from '../shared/protocol';
import { buildRunView, mergeToolCallPage } from './runModel';
import { triggerSkill } from './runFacts';

export interface LoadedRun {
  view: RunView;
  /** The thread the run belongs to, for the link back; absent when no event names one. */
  rootEventId: string | undefined;
}

const EVENT_PAGE_CAP = 20;
const CALLS_PAGE = 50;

/**
 * Everything run detail needs, read in the only order that works: there is no run-detail
 * tool, and `list_lineage` needs a ROOT, which a run id does not carry. So the run's own
 * events come first — they name the root — then the lineage for the run node, then the
 * first page of tool calls.
 */
export async function loadRun(client: EscurelClient, runId: string): Promise<LoadedRun> {
  // Oldest first (the default), which `buildRunView` relies on; `run_id` implies system rows.
  const events: Event[] = [];
  let cursor: string | undefined;
  for (let i = 0; i < EVENT_PAGE_CAP; i += 1) {
    const page = await client.listEvents({ run_id: runId, ...(cursor ? { cursor } : {}) });
    events.push(...page.events);
    // `has_more` says rows follow; `next_cursor` alone is only where this page ended.
    if (!page.has_more || !page.next_cursor) break;
    cursor = page.next_cursor;
  }
  const rootEventId = events.find((e) => e.root_event_id)?.root_event_id ?? undefined;

  // Denial is absence: the node may be missing while the run's own events are readable, and
  // run detail is still worth showing from them.
  let node: LineageNode | undefined;
  let skill: string | undefined;
  if (rootEventId) {
    try {
      const lineage = await client.listLineage({
        root_event_id: rootEventId,
        include: ['runs', 'tool_calls'],
      });
      node = lineage.nodes.find((n) => n.type === 'run' && n.id === runId);
      skill = triggerSkill(lineage.nodes, runId);
    } catch {
      // A denied root does not prevent showing the run's own events.
    }
  }

  const calls = await client.getRunToolCalls({ run_id: runId, limit: CALLS_PAGE });
  const view = mergeToolCallPage(buildRunView(node, events), calls);
  const started = events.find((e) => e.title === 'run-started');
  const runner = started?.provenance?.runner;
  const eventId =
    runner && typeof runner === 'object' ? (runner as Record<string, unknown>).event_id : undefined;
  const enriched = {
    ...view,
    ...(skill ? { skill } : {}),
    ...(typeof eventId === 'string' && eventId ? { triggerEventId: eventId } : {}),
  };
  // The id is known even when neither the node nor an event carried it.
  return { view: enriched.runId ? enriched : { ...enriched, runId }, rootEventId };
}

/**
 * A refetch rereads page one of the tool calls. Keep the later pages the user already asked
 * for, so a live update does not snap a long call list back to its first rows while they are
 * reading down it. Rows are written once and never change, so carrying them is safe.
 */
export function carryCalls(next: RunView, previous: RunView | undefined): RunView {
  if (!previous) return next;
  const reach = (v: RunView) => v.calls.reduce((max, c) => Math.max(max, c.seq), 0);
  if (reach(previous) <= reach(next)) return next;
  const bySeq = new Map(next.calls.map((c) => [c.seq, c]));
  for (const c of previous.calls) if (!bySeq.has(c.seq)) bySeq.set(c.seq, c);
  return {
    ...next,
    calls: [...bySeq.values()].sort((a, b) => a.seq - b.seq),
    nextAfter: previous.nextAfter,
  };
}
