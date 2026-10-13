import type { CaptureEventRequest, Event, EventsPage, ListEventsRequest } from '../client/types';
import { formatAge } from '../shared/time';
import { cleanText } from '../shared/untrustedText';

/** The label a thread close/reopen request is captured under (gateway: `THREAD_CLOSED_LABEL`). */
export const THREAD_CLOSED_LABEL = 'escurel:thread-closed';

export type ThreadCloseAction = 'close' | 'reopen';

export interface ClosedThread {
  rootEventId: string;
  title: string;
  closedBy: string;
  closedAt: string;
  reason?: string | undefined;
}

interface ThreadMark {
  action: ThreadCloseAction;
  at: string;
  eventId: string;
  rootEventId: string;
  rootTitle?: string | undefined;
  requestedBy: string;
  reason?: string | undefined;
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === 'object' ? (value as Record<string, unknown>) : undefined;
}

function asText(value: unknown): string | undefined {
  return typeof value === 'string' && value ? value : undefined;
}

function cleaned(value: unknown): string | undefined {
  const text = asText(value);
  if (!text) return undefined;
  const flat = cleanText(text);
  return flat || undefined;
}

/** The close/reopen mark an event carries, if it is one. The lineage root wins over the body. */
export function threadMark(event: Event): ThreadMark | undefined {
  if (event.label_skill !== THREAD_CLOSED_LABEL) return undefined;
  const block = asRecord(event.provenance?.thread);
  const action = block?.action;
  if (action !== 'close' && action !== 'reopen') return undefined;
  const rootEventId =
    asText(block?.root_event_id) ?? asText(event.root_event_id) ?? undefined;
  if (!rootEventId) return undefined;
  return {
    action,
    at: asText(event.at) ?? '',
    eventId: event.event_id,
    rootEventId,
    // Free text another person typed (or a long title): cleaned and capped where it enters the
    // model, so no later label, tooltip or description can show a bidi override or a flood.
    rootTitle: cleaned(block?.root_title),
    requestedBy: cleaned(block?.requested_by) ?? 'unknown',
    reason: cleaned(block?.reason),
  };
}

/** Later wins: a newer mark replaces an older one for the same root. */
function newer(a: ThreadMark, b: ThreadMark): boolean {
  if (a.at !== b.at) return a.at > b.at;
  return a.eventId > b.eventId;
}

/**
 * The closed threads among `events` (a `list_events{label_skill}` page): the latest mark per
 * root wins, so a reopened thread leaves the group and a re-closed one returns to it. Newest
 * close first.
 */
export function foldClosedThreads(events: Event[]): ClosedThread[] {
  const latest = new Map<string, ThreadMark>();
  for (const event of events) {
    const mark = threadMark(event);
    if (!mark) continue;
    const seen = latest.get(mark.rootEventId);
    if (!seen || newer(mark, seen)) latest.set(mark.rootEventId, mark);
  }
  return [...latest.values()]
    .filter((mark) => mark.action === 'close')
    .sort((a, b) => (b.at === a.at ? (b.eventId > a.eventId ? 1 : -1) : b.at > a.at ? 1 : -1))
    .map((mark) => ({
      rootEventId: mark.rootEventId,
      title: mark.rootTitle ?? '(untitled)',
      closedBy: mark.requestedBy,
      closedAt: mark.at,
      ...(mark.reason ? { reason: mark.reason } : {}),
    }));
}

export interface LoadedClosedThreads {
  threads: ClosedThread[];
  /** True when the page budget ran out before the history did: older closes may be missing. */
  truncated: boolean;
}

/**
 * The closed threads of a tenant, read from the `escurel:thread-closed` listing NEWEST FIRST so the
 * latest mark per root is always among the pages read (oldest-first, a window of the first N marks
 * misses every later reopen and every recent close of a long history). Bounded: at most `maxPages`
 * pages of `pageSize`; `truncated` says when older marks were left unread. A failing page rejects, so
 * the caller decides whether to keep or clear what it showed.
 */
export async function loadClosedThreads(
  listEvents: (req: ListEventsRequest) => Promise<EventsPage>,
  options: { pageSize?: number; maxPages?: number } = {},
): Promise<LoadedClosedThreads> {
  const pageSize = options.pageSize ?? 200;
  const maxPages = options.maxPages ?? 5;
  const events: Event[] = [];
  let cursor: string | undefined;
  let more = false;
  for (let page = 0; page < maxPages; page += 1) {
    const got = await listEvents({
      label_skill: THREAD_CLOSED_LABEL,
      newest_first: true,
      limit: pageSize,
      ...(cursor ? { cursor } : {}),
    });
    events.push(...got.events);
    more = got.has_more === true && !!got.next_cursor;
    if (!more) break;
    cursor = got.next_cursor;
  }
  return { threads: foldClosedThreads(events), truncated: more };
}

/** `Closed by <who> · <age>`, with the reason when the closer left one. */
export function describeClosedThread(closed: ClosedThread, now: Date = new Date()): string {
  const age = closed.closedAt ? formatAge(closed.closedAt, now) : '';
  const who = `Closed by ${closed.closedBy}${age ? ` · ${age}` : ''}`;
  return closed.reason ? `${who} — ${closed.reason}` : who;
}

/**
 * A close/reopen request as the gateway wants it (tools_control.rs): a JSON body on an
 * `escurel:thread-closed` event. The gateway replaces the event's kind, its target page and
 * the `thread` block, and writes `requested_by` from the token, so none of those are sent.
 */
export function buildThreadCloseEvent(
  rootEventId: string,
  action: ThreadCloseAction,
  reason?: string,
): CaptureEventRequest {
  const root = rootEventId.trim();
  if (!root) throw new Error(`${action} needs the thread (a root event id)`);
  const body: Record<string, string> = { action, root_event_id: root };
  const trimmed = reason?.trim();
  if (trimmed) body.reason = trimmed;
  return {
    label_skill: THREAD_CLOSED_LABEL,
    source: 'workbench',
    mime: 'application/json',
    title: action,
    body: JSON.stringify(body),
  };
}

export type ThreadCloseArgument =
  | string
  | { rootEventId?: string | undefined; id?: string | undefined; target?: unknown }
  | undefined;

/**
 * The thread root a close/reopen command acts on. A closed-group row and a bare id name it
 * directly; any other outline row means the outline's own open thread; with neither, there is
 * nothing to act on and the command says where to pick a thread.
 */
export function resolveThreadRoot(
  arg: ThreadCloseArgument,
  outlineRoot: string | undefined,
): string | undefined {
  if (typeof arg === 'string') return arg || undefined;
  if (arg && typeof arg === 'object') {
    const target = asRecord(arg.target);
    const named = asText(target?.rootEventId);
    if (named) return named;
    const direct = asText(arg.rootEventId);
    if (direct) return direct;
    if (asText(arg.id) && outlineRoot) return outlineRoot;
  }
  return outlineRoot;
}
