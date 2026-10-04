// Reads the runs control center's data from the gateway. No `vscode` import: the provider owns the UI.
import type { EscurelClient, Event } from '../client';
import { estimateHeartbeatIntervalMs, ESCUREL_RUNNER_STATUS_INTERVAL_MS } from './runnerModel';
import { foldRuns } from './runsModel';

/** Run lifecycle events loaded so far, newest first by what the gateway returned. */
export interface RunsSnapshot {
  events: Map<string, Event>;
  /** Where the OLDEST loaded page ended: pass it back to read older runs. */
  olderCursor: string | undefined;
  /** Whether older run events lie past what is loaded. */
  hasMoreOlder: boolean;
}

export const emptySnapshot = (): RunsSnapshot => ({
  events: new Map(),
  olderCursor: undefined,
  hasMoreOlder: false,
});

const PAGE = 100;

/** How many finished runs the loaded events describe. */
function endedRuns(events: Iterable<Event>): number {
  let n = 0;
  for (const e of events) if (e.title === 'run-finished') n += 1;
  return n;
}

/**
 * Brings the newest run events into the snapshot: newest pages until one holds nothing new (a refresh
 * is usually a handful of events), or a first load has enough finished runs to fill a page of history.
 */
export async function refreshRunEvents(
  client: EscurelClient,
  snap: RunsSnapshot,
  opts: { wantEnded?: number; maxPages?: number } = {},
): Promise<RunsSnapshot> {
  const first = snap.events.size === 0;
  const wantEnded = opts.wantEnded ?? 25;
  const maxPages = opts.maxPages ?? (first ? 8 : 3);
  const events = new Map(snap.events);
  let cursor: string | undefined;
  let olderCursor = snap.olderCursor;
  let hasMoreOlder: boolean = snap.hasMoreOlder;
  for (let i = 0; i < maxPages; i += 1) {
    const page = await client.listEvents({
      label_skill: 'escurel:run',
      include_system: true,
      newest_first: true,
      limit: PAGE,
      ...(cursor ? { cursor } : {}),
    });
    const rows = page.events ?? [];
    let fresh = 0;
    for (const e of rows) {
      if (!events.has(e.event_id)) {
        events.set(e.event_id, e);
        fresh += 1;
      }
    }
    if (first) {
      olderCursor = page.next_cursor ?? olderCursor;
      hasMoreOlder = !!page.has_more;
    }
    const enough = first ? endedRuns(events.values()) >= wantEnded : fresh === 0;
    if (rows.length === 0 || !page.has_more || !page.next_cursor || enough) break;
    cursor = page.next_cursor;
  }
  return { events, olderCursor, hasMoreOlder };
}

/** Reads older run events until `more` more finished runs are loaded (or the gateway has no more). */
export async function loadOlderRunEvents(
  client: EscurelClient,
  snap: RunsSnapshot,
  more = 25,
): Promise<RunsSnapshot> {
  if (!snap.hasMoreOlder || !snap.olderCursor) return snap;
  const events = new Map(snap.events);
  const target = endedRuns(events.values()) + more;
  let cursor: string | undefined = snap.olderCursor;
  let olderCursor = snap.olderCursor;
  let hasMoreOlder: boolean = snap.hasMoreOlder;
  for (let i = 0; i < 8 && cursor; i += 1) {
    const page = await client.listEvents({
      label_skill: 'escurel:run',
      include_system: true,
      newest_first: true,
      limit: PAGE,
      cursor,
    });
    for (const e of page.events ?? []) events.set(e.event_id, e);
    olderCursor = page.next_cursor ?? olderCursor;
    hasMoreOlder = !!page.has_more;
    cursor = page.has_more ? page.next_cursor : undefined;
    if (endedRuns(events.values()) >= target) break;
  }
  return { events, olderCursor, hasMoreOlder };
}

export interface StatusRead {
  event: Event | undefined;
  intervalMs: number;
}

/** The runner's newest status event, and the heartbeat interval its recent rows show. */
export async function readRunnerStatus(client: EscurelClient): Promise<StatusRead> {
  const page = await client.listEvents({
    label_skill: 'escurel:runner-status',
    newest_first: true,
    include_system: true,
    limit: 8,
  });
  return {
    event: page.events?.[0],
    intervalMs: page.events?.length
      ? estimateHeartbeatIntervalMs(page.events)
      : ESCUREL_RUNNER_STATUS_INTERVAL_MS,
  };
}

/**
 * The skill each run executes is the label of the event that TRIGGERED it, and the run events only
 * name that event. One lookup per trigger, cached for the life of the view; a trigger that cannot be
 * read (aged out, not visible) is remembered as unknown so it is not asked again.
 */
export async function resolveSkills(
  client: EscurelClient,
  triggerIds: readonly string[],
  cache: Map<string, string>,
  concurrency = 4,
): Promise<boolean> {
  const todo = [...new Set(triggerIds)].filter((id) => id && !cache.has(id));
  if (todo.length === 0) return false;
  let next = 0;
  const worker = async () => {
    while (next < todo.length) {
      const id = todo[next++]!;
      try {
        const page = await client.listEvents({ event_id: id, include_system: true, limit: 1 });
        const label = page.events?.[0]?.label_skill ?? '';
        cache.set(id, label.startsWith('escurel:') ? '' : label);
      } catch {
        // Not cached: a transient failure is retried on the next refresh.
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(concurrency, todo.length) }, worker));
  return true;
}

/** What `foldRuns` needs from a snapshot and a status read. */
export function recordsFrom(
  snap: RunsSnapshot,
  nowMs: number,
  liveRunIds: ReadonlySet<string> | undefined,
  skillCache: ReadonlyMap<string, string>,
) {
  const skillByEvent = new Map<string, string>();
  for (const [id, skill] of skillCache) if (skill) skillByEvent.set(id, skill);
  return foldRuns([...snap.events.values()], { nowMs, liveRunIds, skillByEvent });
}
