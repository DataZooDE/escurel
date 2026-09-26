import type { Event } from '../client';
import { pageSlug } from '../shared/pageId';

/**
 * An event's age matters more than its clock time in a queue you scan, so
 * the row carries "5m ago" rather than a timestamp. `now` is injectable
 * because a test that reads the wall clock is a test that fails at midnight.
 */
export function formatRelativeTime(at: string | null | undefined, now = Date.now()): string {
  if (!at) return '';
  const timestamp = new Date(at).getTime();
  if (Number.isNaN(timestamp)) return '';

  const diffSec = Math.floor((now - timestamp) / 1000);
  if (diffSec < 60) return 'just now';

  const diffMin = Math.floor(diffSec / 60);
  if (diffMin < 60) return `${diffMin}m ago`;

  const diffHours = Math.floor(diffMin / 60);
  if (diffHours < 24) return `${diffHours}h ago`;

  const diffDays = Math.floor(diffHours / 24);
  if (diffDays < 30) return `${diffDays}d ago`;

  const diffMonths = Math.floor(diffDays / 30);
  if (diffMonths < 12) return `${diffMonths}mo ago`;

  const diffYears = Math.floor(diffDays / 365);
  return `${diffYears}y ago`;
}

/** One inbox event as the tree shows it. */
export interface InboxRow {
  kind: 'event';
  event: Event;
  label: string;
  description: string;
  tooltip: string;
  pageId?: string;
  body?: string | null;
}

/**
 * The label leads with the label_skill because that is what routes the
 * event; the title is the human part, and the event id stands in when a
 * capture carried no title.
 */
export function inboxRow(event: Event, now?: number): InboxRow {
  const title = event.title?.trim();
  const label = title
    ? `${event.label_skill} · ${title}`
    : `${event.label_skill} · ${event.event_id}`;

  const parts: string[] = [];
  if (event.instance_page_id) {
    parts.push(pageSlug(event.instance_page_id));
  }
  const time = formatRelativeTime(event.at, now);
  if (time) {
    parts.push(time);
  }
  const description = parts.join(' · ');

  return {
    kind: 'event',
    event,
    label,
    description,
    tooltip: `${label}${description ? ` (${description})` : ''}`,
    pageId: event.instance_page_id ?? undefined,
    body: event.body,
  };
}

/**
 * Newest first (SPEC §3.2). `list_inbox` answers in id order, so the view
 * sorts; an undated capture sorts last rather than pretending to be old,
 * and the id breaks ties so the order never flickers between reads.
 */
export function sortEventsNewestFirst(events: Event[]): Event[] {
  return [...events].sort((a, b) => {
    const timeA = a.at ? new Date(a.at).getTime() : 0;
    const timeB = b.at ? new Date(b.at).getTime() : 0;
    if (timeA !== timeB) return timeB - timeA;
    return b.event_id.localeCompare(a.event_id);
  });
}

/** The whole inbox page as rows, newest first. */
export function buildInboxRows(events: Event[], now?: number): InboxRow[] {
  return sortEventsNewestFirst(events).map((e) => inboxRow(e, now));
}
