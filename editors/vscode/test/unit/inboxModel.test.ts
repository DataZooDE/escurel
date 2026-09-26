import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client';
import {
  buildInboxRows,
  formatRelativeTime,
  inboxRow,
  sortEventsNewestFirst,
} from '../../src/views/inboxModel';
import { pageSlug } from '../../src/shared/pageId';
import { fixture } from './mockGateway';

const inboxFixture = (
  fixture('list_inbox').response as { result: { structuredContent: { events: Event[] } } }
).result.structuredContent.events;

describe('inboxModel', () => {
  const baseNow = new Date('2026-09-25T12:00:00Z').getTime();

  it('pageSlug reads flat and nested page ids, and keeps a name the skill does not prefix', () => {
    expect(pageSlug('markdown/instances/customer__alpina-biotech.md')).toBe('alpina-biotech');
    expect(pageSlug('markdown/instances/customer/alpina-biotech.md')).toBe('alpina-biotech');
    expect(pageSlug('markdown/skills/customer.md')).toBe('customer');
    // With the skill known, only that exact prefix is a separator.
    expect(pageSlug('markdown/instances/customer__acme.md', 'customer')).toBe('acme');
    expect(pageSlug('markdown/instances/other__acme.md', 'customer')).toBe('other__acme');
  });

  describe('formatRelativeTime', () => {
    it('formats seconds, minutes, hours, and days relative to now', () => {
      expect(formatRelativeTime(null, baseNow)).toBe('');
      expect(formatRelativeTime(undefined, baseNow)).toBe('');
      expect(formatRelativeTime('invalid-date', baseNow)).toBe('');

      // < 60s
      expect(formatRelativeTime('2026-09-25T11:59:30Z', baseNow)).toBe('just now');
      // minutes
      expect(formatRelativeTime('2026-09-25T11:45:00Z', baseNow)).toBe('15m ago');
      // hours
      expect(formatRelativeTime('2026-09-25T08:00:00Z', baseNow)).toBe('4h ago');
      // days
      expect(formatRelativeTime('2026-09-22T12:00:00Z', baseNow)).toBe('3d ago');
    });
  });

  describe('inboxRow', () => {
    it('formats an event with instance_page_id showing skill, title, slug, and relative time', () => {
      const event: Event = {
        event_id: 'ev-1',
        at: '2026-09-25T10:00:00Z',
        source: 'workbench',
        mime: 'text/plain',
        label_skill: 'customer',
        instance_page_id: 'markdown/instances/customer__alpina-biotech.md',
        status: 'inbox',
        title: 'New customer registration',
        body: 'Details here',
        provenance: null,
        kind: 'user',
        root_event_id: 'ev-1',
        run_id: null,
      };

      const row = inboxRow(event, baseNow);
      expect(row.kind).toBe('event');
      expect(row.label).toBe('customer · New customer registration');
      expect(row.description).toBe('alpina-biotech · 2h ago');
      expect(row.pageId).toBe('markdown/instances/customer__alpina-biotech.md');
      expect(row.event).toBe(event);
    });

    it('formats an event without instance_page_id showing only relative time in description', () => {
      const event: Event = {
        event_id: 'ev-2',
        at: '2026-09-25T11:00:00Z',
        source: 'gmail',
        mime: 'message/rfc822',
        label_skill: 'email',
        instance_page_id: null,
        status: 'inbox',
        title: 'RFQ inquiry',
        body: 'Inquiry body',
        provenance: null,
        kind: 'user',
        root_event_id: 'ev-2',
        run_id: null,
      };

      const row = inboxRow(event, baseNow);
      expect(row.label).toBe('email · RFQ inquiry');
      expect(row.description).toBe('1h ago');
      expect(row.pageId).toBeUndefined();
    });

    it('falls back to event_id when title is missing or empty', () => {
      const event: Event = {
        event_id: 'ev-3',
        at: '2026-09-25T11:30:00Z',
        source: 'agent',
        mime: null,
        label_skill: 'doc',
        instance_page_id: null,
        status: 'inbox',
        title: null,
        body: null,
        provenance: null,
        kind: 'user',
        root_event_id: null,
        run_id: null,
      };

      const row = inboxRow(event, baseNow);
      expect(row.label).toBe('doc · ev-3');
    });
  });

  describe('sortEventsNewestFirst', () => {
    it('sorts events in descending order by `at` timestamp', () => {
      const e1: Event = { ...inboxFixture[0]!, event_id: 'e1', at: '2026-01-01T00:00:00Z' };
      const e2: Event = { ...inboxFixture[0]!, event_id: 'e2', at: '2026-03-01T00:00:00Z' };
      const e3: Event = { ...inboxFixture[0]!, event_id: 'e3', at: '2026-02-01T00:00:00Z' };

      const sorted = sortEventsNewestFirst([e1, e2, e3]);
      expect(sorted.map((e) => e.event_id)).toEqual(['e2', 'e3', 'e1']);
    });
  });

  describe('buildInboxRows', () => {
    it('maps and sorts the inbox fixture events newest first', () => {
      const rows = buildInboxRows(inboxFixture, baseNow);
      expect(rows.length).toBe(inboxFixture.length);
      expect(rows[0]!.event.event_id).toBe('01M38SJJEMTNW3XTYQ6YJDPJ36');
      expect(rows[0]!.label).toBe('customer · hello');
      expect(rows[0]!.pageId).toBe('markdown/instances/customer__alpina-biotech.md');
    });
  });
});
