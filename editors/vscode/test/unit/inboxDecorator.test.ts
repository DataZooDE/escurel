import { describe, expect, it } from 'vitest';
import * as vscode from 'vscode';
import type { Event } from '../../src/client';
import {
  InboxDecoratorRegistry,
  decorateRows,
  emptyInboxMessage,
} from '../../src/api/inbox';
import { InboxTree } from '../../src/views/inbox';
import { buildInboxRows } from '../../src/views/inboxModel';

const ev = (id: string, skill: string, over: Partial<Event> = {}): Event => ({
  event_id: id,
  at: '2026-09-25T11:00:00Z',
  source: null,
  mime: null,
  label_skill: skill,
  instance_page_id: null,
  status: 'inbox',
  title: `title ${id}`,
  body: null,
  provenance: { captured_by: 'user:a', n: 5 } as never,
  kind: 'user',
  root_event_id: null,
  run_id: null,
  ...over,
});
const now = new Date('2026-09-25T12:00:00Z').getTime();

describe('InboxDecoratorRegistry', () => {
  it('validates the skill and needs a decorate function or an empty message', () => {
    const reg = new InboxDecoratorRegistry();
    expect(() => reg.register('bad id', { decorate: () => undefined })).toThrow(/skill/);
    expect(() => reg.register('acme-request', {})).toThrow(/at least one/);
    expect(() => reg.register('acme-request', { decorate: 'x' as never })).toThrow(/function/);
    expect(() => reg.register('acme-request', { emptyMessage: 5 as never })).toThrow(/emptyMessage/);
    const d = reg.register('acme-request', { decorate: () => undefined });
    expect(reg.size).toBe(1);
    d.dispose();
    expect(reg.size).toBe(0);
  });
});

describe('decorateRows', () => {
  it('decorates only rows of the registered skill and hands the decorator strings only', async () => {
    const reg = new InboxDecoratorRegistry();
    const seen: unknown[] = [];
    reg.register('acme-request', {
      decorate: (row, rows) => {
        seen.push({ row, count: rows.length });
        return { badge: 'Answered', description: 'by the agent', status: 'answered' };
      },
    });
    const rows = buildInboxRows([ev('1', 'acme-request'), ev('2', 'other')], now);
    const out = await decorateRows(reg, rows, now);
    const first = out.rows.find((r) => r.event.event_id === '1')!;
    const other = out.rows.find((r) => r.event.event_id === '2')!;
    expect(first.status).toBe('answered');
    expect(first.description).toBe('Answered · by the agent');
    expect(other.status).toBeUndefined();
    expect(other.description).toContain('other');
    expect(seen).toHaveLength(1);
    expect(seen[0]).toMatchObject({
      count: 2,
      row: { eventId: '1', labelSkill: 'acme-request', title: 'title 1', provenance: { captured_by: 'user:a' } },
    });
    // a number in provenance is not handed out
    expect((seen[0] as { row: { provenance: object } }).row.provenance).not.toHaveProperty('n');
  });

  it('cleans what a decorator returns and drops what is not the schema', async () => {
    const reg = new InboxDecoratorRegistry();
    reg.register('acme-request', {
      decorate: () => ({ badge: 'Open‮'.repeat(30), description: 5 as never, status: 'bogus' as never }),
    });
    const out = await decorateRows(reg, buildInboxRows([ev('1', 'acme-request')], now), now);
    const row = out.rows[0]!;
    expect(row.status).toBeUndefined();
    expect(row.badge!.length).toBeLessThanOrEqual(24);
    expect(row.badge).not.toContain('‮');
  });

  it('counts the open rows and sorts a decided row after an open one', async () => {
    const reg = new InboxDecoratorRegistry();
    reg.register('acme-request', {
      decorate: (r) => ({ status: r.eventId === '2' ? 'answered' : 'open' }),
    });
    const rows = buildInboxRows(
      [ev('1', 'acme-request', { at: '2026-09-25T10:00:00Z' }), ev('2', 'acme-request', { at: '2026-09-25T11:00:00Z' })],
      now,
    );
    const out = await decorateRows(reg, rows, now);
    expect(out.rows.map((r) => r.event.event_id)).toEqual(['1', '2']);
    expect(out.open).toBe(1);
  });

  it('hides a row when the decorator says so', async () => {
    const reg = new InboxDecoratorRegistry();
    reg.register('acme-receipt', { decorate: () => ({ hidden: true }) });
    const out = await decorateRows(
      reg,
      buildInboxRows([ev('1', 'acme-receipt'), ev('2', 'x')], now),
      now,
    );
    expect(out.rows.map((r) => r.event.event_id)).toEqual(['2']);
  });

  it('a decorator that throws or hangs leaves its rows as they were', async () => {
    const reg = new InboxDecoratorRegistry();
    reg.register('a', {
      decorate: () => {
        throw new Error('boom');
      },
    });
    reg.register('b', { decorate: () => new Promise(() => undefined) });
    const rows = buildInboxRows([ev('1', 'a'), ev('2', 'b')], now);
    const out = await decorateRows(reg, rows, now, 20);
    expect(out.rows.map((r) => r.description)).toEqual(rows.map((r) => r.description));
  });

  it('with no decorator the rows come back untouched', async () => {
    const rows = buildInboxRows([ev('1', 'a')], now);
    const out = await decorateRows(new InboxDecoratorRegistry(), rows, now);
    expect(out.rows).toEqual(rows);
    expect(out.open).toBe(0);
  });
});

describe('emptyInboxMessage', () => {
  it('picks the language of the window, then the language family, then English', () => {
    const reg = new InboxDecoratorRegistry();
    reg.register('acme-request', {
      emptyMessage: { en: 'No requests yet.', de: 'Noch keine Anfragen.' },
    });
    expect(emptyInboxMessage(reg, 'de')).toBe('Noch keine Anfragen.');
    expect(emptyInboxMessage(reg, 'de-AT')).toBe('Noch keine Anfragen.');
    expect(emptyInboxMessage(reg, 'fr')).toBe('No requests yet.');
  });

  it('takes a plain string as is, cleans it, and is undefined when nobody contributed one', () => {
    const reg = new InboxDecoratorRegistry();
    expect(emptyInboxMessage(reg, 'en')).toBeUndefined();
    reg.register('x', { emptyMessage: 'Nothing here‮.' });
    expect(emptyInboxMessage(reg, 'en')).toBe('Nothing here.');
  });
});

describe('InboxTree with decorators', () => {
  function tree(events: Event[], language = 'en') {
    const reg = new InboxDecoratorRegistry();
    const client = { listInbox: async () => ({ events }) };
    const t = new InboxTree(() => client as never, reg, () => language);
    const view: { message?: string; badge?: { value: number; tooltip: string } } = {};
    t.attach(view as never);
    return { t, reg, view };
  }

  it('shows decorated rows, the open count as the view badge and no message', async () => {
    const { t, reg, view } = tree([ev('1', 'acme-request')]);
    reg.register('acme-request', { decorate: () => ({ badge: 'Open', status: 'open' }) });
    const rows = (await t.getChildren()) as { description: string; status?: string }[];
    expect(rows[0]!.status).toBe('open');
    expect(rows[0]!.description.startsWith('Open')).toBe(true);
    expect(view.badge?.value).toBe(1);
    expect(view.message).toBeUndefined();
    const item = t.getTreeItem(rows[0] as never);
    expect(item.iconPath).toBeDefined();
  });

  it('says why an empty inbox is empty, in the language of the window', async () => {
    const { t, reg, view } = tree([], 'de');
    reg.register('acme-request', { emptyMessage: { en: 'No requests yet.', de: 'Noch keine Anfragen.' } });
    await t.getChildren();
    expect(view.message).toBe('Noch keine Anfragen.');
  });

  it('is unchanged without any decorator: no badge, no message', async () => {
    const { t, view } = tree([ev('1', 'x')]);
    const rows = (await t.getChildren()) as { description: string }[];
    expect(rows[0]!.description).toContain('x');
    expect(view.badge).toBeUndefined();
    expect(view.message).toBeUndefined();
    void vscode;
  });
});
