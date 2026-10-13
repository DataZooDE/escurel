import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client/types';
import {
  THREAD_CLOSED_LABEL,
  buildThreadCloseEvent,
  describeClosedThread,
  foldClosedThreads,
  resolveThreadRoot,
} from '../../src/views/closedThreads';
import { closedGroupRow } from '../../src/views/threadsModel';

function mark(
  eventId: string,
  root: string,
  action: 'close' | 'reopen',
  at: string,
  extra?: { title?: string; by?: string; reason?: string },
): Event {
  return {
    event_id: eventId,
    at,
    source: 'workbench',
    mime: 'application/json',
    label_skill: THREAD_CLOSED_LABEL,
    instance_page_id: 'markdown/instances/community_member/alice.md',
    status: 'processed',
    title: action,
    body: '{}',
    provenance: {
      thread: {
        action,
        root_event_id: root,
        root_title: extra?.title ?? 'a question',
        requested_by: extra?.by ?? 'whatsapp:111',
        ...(extra?.reason ? { reason: extra.reason } : {}),
      },
    },
    kind: 'system',
    root_event_id: root,
    run_id: null,
  };
}

describe('foldClosedThreads', () => {
  it('lists a closed thread with its title, closer and time', () => {
    const [closed] = foldClosedThreads([
      mark('e1', 'root-a', 'close', '2026-10-12T10:00:00Z', { reason: 'done' }),
    ]);
    expect(closed).toMatchObject({
      rootEventId: 'root-a',
      title: 'a question',
      closedBy: 'whatsapp:111',
      closedAt: '2026-10-12T10:00:00Z',
      reason: 'done',
    });
  });

  it('ignores other labels and marks without a known action', () => {
    const other = { ...mark('e1', 'root-a', 'close', '2026-10-12T10:00:00Z'), label_skill: 'other' };
    const noAction = {
      ...mark('e2', 'root-b', 'close', '2026-10-12T10:00:00Z'),
      provenance: { thread: { action: 'hide' } },
    };
    expect(foldClosedThreads([other, noAction])).toEqual([]);
  });

  it('a reopen removes the thread; a later close returns it', () => {
    const events = [
      mark('e1', 'root-a', 'close', '2026-10-12T10:00:00Z'),
      mark('e2', 'root-a', 'reopen', '2026-10-12T11:00:00Z'),
    ];
    expect(foldClosedThreads(events)).toEqual([]);
    expect(foldClosedThreads([...events, mark('e3', 'root-a', 'close', '2026-10-12T12:00:00Z')])).toHaveLength(1);
  });

  it('the latest mark wins even when pages arrive oldest-first', () => {
    const events = [
      mark('e2', 'root-a', 'reopen', '2026-10-12T11:00:00Z'),
      mark('e1', 'root-a', 'close', '2026-10-12T10:00:00Z'),
    ];
    expect(foldClosedThreads(events)).toEqual([]);
  });

  it('keeps open and closed roots apart, newest close first', () => {
    const events = [
      mark('e1', 'root-a', 'close', '2026-10-12T10:00:00Z', { title: 'first' }),
      mark('e2', 'root-b', 'close', '2026-10-12T12:00:00Z', { title: 'second' }),
      mark('e3', 'root-c', 'close', '2026-10-12T11:00:00Z', { title: 'third' }),
      mark('e4', 'root-c', 'reopen', '2026-10-12T13:00:00Z', { title: 'third' }),
    ];
    expect(foldClosedThreads(events).map((c) => c.rootEventId)).toEqual(['root-b', 'root-a']);
  });

  it('falls back to the lineage root and skips a mark with no root at all', () => {
    const viaLineage = {
      ...mark('e1', 'root-a', 'close', '2026-10-12T10:00:00Z'),
      provenance: { thread: { action: 'close', requested_by: 'whatsapp:111' } },
    };
    const noRoot = { ...viaLineage, event_id: 'e2', root_event_id: null };
    expect(foldClosedThreads([viaLineage]).map((c) => c.rootEventId)).toEqual(['root-a']);
    expect(foldClosedThreads([noRoot])).toEqual([]);
  });
});

describe('describeClosedThread', () => {
  it('names who closed it and how long ago, with the reason when left', () => {
    const now = new Date('2026-10-12T12:00:00Z');
    expect(
      describeClosedThread(
        { rootEventId: 'r', title: 't', closedBy: 'alice', closedAt: '2026-10-12T10:00:00Z' },
        now,
      ),
    ).toBe('Closed by alice · 2 h ago');
    expect(
      describeClosedThread(
        {
          rootEventId: 'r',
          title: 't',
          closedBy: 'alice',
          closedAt: '2026-10-12T10:00:00Z',
          reason: 'done',
        },
        now,
      ),
    ).toBe('Closed by alice · 2 h ago — done');
  });
});

describe('buildThreadCloseEvent', () => {
  it('builds the body the gateway authorises, stamping nothing itself', () => {
    expect(buildThreadCloseEvent('root-a', 'close', ' done ')).toEqual({
      label_skill: THREAD_CLOSED_LABEL,
      source: 'workbench',
      mime: 'application/json',
      title: 'close',
      body: JSON.stringify({ action: 'close', root_event_id: 'root-a', reason: 'done' }),
    });
    expect(buildThreadCloseEvent('root-a', 'reopen')).toEqual({
      label_skill: THREAD_CLOSED_LABEL,
      source: 'workbench',
      mime: 'application/json',
      title: 'reopen',
      body: JSON.stringify({ action: 'reopen', root_event_id: 'root-a' }),
    });
  });

  it('refuses an empty root before anything is sent', () => {
    expect(() => buildThreadCloseEvent('  ', 'close')).toThrow(/needs the thread/);
  });
});

describe('resolveThreadRoot', () => {
  it('takes a bare id, a named root, or a closed-group row directly', () => {
    expect(resolveThreadRoot('root-a', undefined)).toBe('root-a');
    expect(resolveThreadRoot({ rootEventId: 'root-a' }, undefined)).toBe('root-a');
    expect(
      resolveThreadRoot(
        { id: 'closed:root-a', target: { open: 'thread', rootEventId: 'root-a' } },
        'other',
      ),
    ).toBe('root-a');
  });

  it('an outline row means the outline’s own open thread', () => {
    expect(resolveThreadRoot({ id: 'node-1' }, 'root-a')).toBe('root-a');
    expect(resolveThreadRoot(undefined, 'root-a')).toBe('root-a');
  });

  it('with no thread anywhere there is nothing to act on', () => {
    expect(resolveThreadRoot(undefined, undefined)).toBeUndefined();
    expect(resolveThreadRoot('', undefined)).toBeUndefined();
    expect(resolveThreadRoot({ id: '' }, undefined)).toBeUndefined();
  });
});

describe('closedGroupRow', () => {
  it('is absent while nothing is closed', () => {
    expect(closedGroupRow([])).toBeUndefined();
  });

  it('groups closed threads collapsed at the end, opening the thread on click', () => {
    const group = closedGroupRow([
      { rootEventId: 'root-a', title: 'first', closedBy: 'alice', closedAt: '2026-10-12T10:00:00Z' },
      { rootEventId: 'root-b', title: 'second', closedBy: 'bob', closedAt: '2026-10-12T11:00:00Z' },
    ])!;
    expect(group.label).toBe('Closed');
    expect(group.description).toBe('2 threads');
    expect(group.contextValue).toBe('escurel.closedThreads');
    expect(group.collapsibleState).toBe('collapsed');
    expect(group.children.map((c) => c.contextValue)).toEqual([
      'escurel.closedThread',
      'escurel.closedThread',
    ]);
    expect(group.children[0]?.target).toEqual({ open: 'thread', rootEventId: 'root-a' });
    expect(group.children[0]?.label).toBe('first');
  });
});

describe('untrusted text on a closed row', () => {
  it('is cleaned and capped before it reaches a label or description', () => {
    const rlo = '\u202e';
    const evil = mark('E1', 'R1', 'close', '2026-10-12T10:00:00Z', {
      title: `evil${rlo}gnp.exe${'x'.repeat(5000)}`,
      by: `bob${rlo}\u0000`,
      reason: `why${rlo}${'y'.repeat(5000)}`,
    });
    const [closed] = foldClosedThreads([evil]);
    for (const text of [closed!.title, closed!.closedBy, closed!.reason ?? '']) {
      expect([...text].some((ch) => ch === '\u202e' || ch === '\u0000')).toBe(false);
      expect(text.length).toBeLessThanOrEqual(400);
    }
    const row = closedGroupRow([closed!])!;
    expect(row.children[0]!.label.length).toBeLessThanOrEqual(400);
    expect(row.children[0]!.description.length).toBeLessThanOrEqual(900);
  });

  it('takes the thread to open from the mark, never from a forged id in the title', () => {
    const e = mark('E2', 'R2', 'close', '2026-10-12T10:00:00Z', { title: 'R9999 open me' });
    const row = closedGroupRow(foldClosedThreads([e]))!;
    expect(row.children[0]!.target).toEqual({ open: 'thread', rootEventId: 'R2' });
  });
});
