import type { Event } from '../client/types';
import type { ThreadStrip } from './protocol';
import { parseGatewayTime } from './time';

/**
 * Where a page came from: the newest run that FINISHED against it, and the thread that run
 * belongs to (SPEC §3.4 "a thread strip: the root event → run that produced this version").
 *
 * Only `run-finished` counts. A `run-started` row says a run is underway, not that it wrote
 * this version, and claiming so would put a thread link on a page the run may never change.
 * Events may arrive in either order, so "newest" is decided by time, not position.
 */
export function buildThreadStrip(events: readonly Event[]): ThreadStrip | undefined {
  let best: { at: number; event: Event } | undefined;
  for (const e of events) {
    if (e.label_skill !== 'escurel:run' || e.title !== 'run-finished' || !e.run_id) continue;
    const at = parseGatewayTime(e.at)?.getTime() ?? 0;
    if (!best || at > best.at) best = { at, event: e };
  }
  if (!best) return undefined;
  const { event } = best;
  const runner = (event.provenance as { runner?: { root_event_id?: unknown } } | undefined)?.runner;
  const rootEventId =
    event.root_event_id ??
    (typeof runner?.root_event_id === 'string' ? runner.root_event_id : undefined);
  if (!rootEventId) return undefined;

  let runStatus = 'processed';
  try {
    const body: unknown = JSON.parse(event.body ?? '{}');
    const status = (body as { status?: unknown }).status;
    if (typeof status === 'string') runStatus = status;
  } catch {
    // A body that is not JSON leaves the default: the run finished, and that is what matters.
  }
  return { rootEventId, runId: event.run_id!, runStatus };
}

const STRIP_PAGE_CAP = 10;

/**
 * The strip for a page, reading its events newest first and following the cursor until a
 * finished run turns up. A busy page can have more than one page of newer events (every
 * review transition is one) between now and the run that wrote it, and stopping at the first
 * page would hide that run. The cap keeps an old page with no runs from reading its whole
 * history; a failed read is no strip, because the strip is an addition to the page.
 */
export async function findThreadStrip(
  readPage: (
    cursor?: string,
  ) => Promise<{ events: Event[]; next_cursor?: string; has_more?: boolean }>,
  maxPages = STRIP_PAGE_CAP,
): Promise<ThreadStrip | undefined> {
  try {
    let cursor: string | undefined;
    for (let i = 0; i < maxPages; i += 1) {
      const page = await readPage(cursor);
      const strip = buildThreadStrip(page.events);
      if (strip) return strip;
      if (!page.has_more || !page.next_cursor) return undefined;
      cursor = page.next_cursor;
    }
  } catch {
    // Fall through: no strip.
  }
  return undefined;
}
