import type { EscurelClient, ListLineageResponse } from '../client';
import { foldLineage, type FoldedLineage } from './threadModel';

/** A thread of thousands of events is still a few dozen pages at the default size. */
const DEFAULT_MAX_PAGES = 200;

export type LoadedThread = FoldedLineage & {
  /** True when the page cap stopped the read: the view is partial and must say so. */
  truncated: boolean;
};

/**
 * The whole lineage under `rootEventId`, every page merged.
 *
 * `next_cursor` pages only the lineage's EVENTS; drafts and changesets are recomputed in
 * full on every page, which is why the merge (`foldLineage`) works by id and cannot be a
 * concatenation. The cap is a circuit breaker against a cursor that never ends, and it is
 * reported rather than silent: a thread that looks complete and is not is worse than one
 * that says it is cut short.
 */
export async function loadThread(
  client: EscurelClient,
  rootEventId: string,
  opts: { maxPages?: number } = {},
): Promise<LoadedThread> {
  const maxPages = opts.maxPages ?? DEFAULT_MAX_PAGES;
  const pages: ListLineageResponse[] = [];
  let cursor: string | undefined;
  let truncated = false;
  for (;;) {
    const page = await client.listLineage({
      root_event_id: rootEventId,
      // `tool_calls` adds the per-run call summary, so a card can say "14 tool calls" without
      // a request per run.
      include: ['events', 'runs', 'drafts', 'tool_calls'],
      ...(cursor ? { cursor } : {}),
    });
    pages.push(page);
    if (!page.next_cursor) break;
    if (pages.length >= maxPages) {
      truncated = true;
      break;
    }
    cursor = page.next_cursor;
  }
  return { ...foldLineage(pages), truncated };
}
