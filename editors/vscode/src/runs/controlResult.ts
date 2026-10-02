import type { Event } from '../client/types';
import { EscurelError } from '../client/errors';

export interface ControlResult {
  action: string;
  outcome: string;
  runId: string | null;
  detail?: string | null;
  newRunId?: string;
  requestEventId?: string;
}

export function parseControlResult(event: Event): ControlResult | undefined {
  if (event.label_skill !== 'escurel:run-control-result' || !event.body) return undefined;
  try {
    const body: unknown = JSON.parse(event.body);
    if (!body || typeof body !== 'object') return undefined;
    const b = body as Record<string, unknown>;
    if (typeof b.action !== 'string' || typeof b.outcome !== 'string') return undefined;
    const control = event.provenance?.control as Record<string, unknown> | undefined;
    return {
      action: b.action,
      outcome: b.outcome,
      runId: typeof b.run_id === 'string' ? b.run_id : null,
      detail: typeof b.detail === 'string' ? b.detail : null,
      newRunId: typeof b.new_run_id === 'string' ? b.new_run_id : undefined,
      requestEventId:
        typeof control?.request_event_id === 'string' ? control.request_event_id : undefined,
    };
  } catch {
    return undefined;
  }
}

export function describeOutcome(result: ControlResult): string {
  switch (result.outcome) {
    case 'cancelled':
      return 'Run cancelled.';
    case 'not_live':
      return `That run is not running any more${result.detail ? ` (${result.detail})` : ''}.`;
    case 'requeued': {
      // The runner answers a retry and a requeue the same way; the person asked for one of them.
      const done = result.action === 'retry' ? 'Retried' : 'Requeued';
      return result.newRunId ? `${done}; a new run has started.` : `${done}.`;
    }
    case 'paused':
      return 'Dispatch paused.';
    case 'resumed':
      return 'Dispatch resumed.';
    case 'refused':
      return result.detail
        ? `The runner refused: ${result.detail}.`
        : 'The runner refused that request.';
    default:
      return result.outcome;
  }
}

export function matchResult(
  events: readonly Event[],
  request: { eventId: string; action: string; runId?: string },
): ControlResult | undefined {
  for (const event of events) {
    const result = parseControlResult(event);
    if (
      result?.action === request.action &&
      result.requestEventId === request.eventId &&
      (!request.runId || result.runId === request.runId)
    )
      return result;
  }
  return undefined;
}

export function describeControlRefusal(error: unknown): string {
  if (error instanceof EscurelError && error.kind === 'event_not_found')
    return 'No such run, or not yours to control.';
  return error instanceof Error ? error.message : String(error);
}

/**
 * Look for the runner's answer to `request`, newest results first, a few pages deep. The newest
 * page alone can miss it: a runner that works through a batch of requests writes many results
 * before the next poll, and the one asked for would sit past the first page for good.
 */
export async function findControlResult(
  fetchPage: (cursor?: string) => Promise<{ events: Event[]; next_cursor?: string | null }>,
  request: { eventId: string; action: string; runId?: string },
  maxPages = 5,
): Promise<ControlResult | undefined> {
  let cursor: string | undefined;
  for (let page = 0; page < maxPages; page += 1) {
    const { events, next_cursor } = await fetchPage(cursor);
    const found = matchResult(events, request);
    if (found) return found;
    if (!next_cursor) return undefined;
    cursor = next_cursor;
  }
  return undefined;
}
