import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { Event } from '../../src/client/types';
import { LiveRefresher, staleViews } from '../../src/live';

function makeEvent(overrides: Partial<Event> = {}): Event {
  return {
    event_id: 'ev-test-1',
    at: '2026-09-26T10:00:00.000000Z',
    source: 'workbench',
    mime: 'text/plain',
    label_skill: 'customer',
    instance_page_id: null,
    status: 'inbox',
    title: 'Customer event',
    body: null,
    provenance: null,
    kind: 'user',
    root_event_id: null,
    run_id: null,
    ...overrides,
  };
}

describe('staleViews (live event routing)', () => {
  it('a review event marks Awaiting stale and not Inbox', () => {
    // Review transitions emitted by the gateway carry label_skill: 'escurel:review'
    // and reflect draft/changeset lifecycles that live exclusively in Awaiting you.
    const titles = [
      'draft-created',
      'draft-promoted',
      'draft-discarded',
      'changeset-promoted',
      'changeset-discarded',
    ];
    for (const title of titles) {
      const event = makeEvent({
        event_id: `ev-${title}`,
        label_skill: 'escurel:review',
        kind: 'system',
        title,
      });
      const stale = staleViews(event);
      expect(stale).toEqual({ inbox: false, awaiting: true });
    }
  });

  it('an ordinary event marks the Inbox stale and not Awaiting', () => {
    // Ordinary inbox items are unprocessed user-scoped events (list_inbox).
    const userInboxEvent = makeEvent({
      event_id: 'ev-user-1',
      label_skill: 'customer',
      kind: 'user',
      status: 'inbox',
    });
    expect(staleViews(userInboxEvent)).toEqual({ inbox: true, awaiting: false });

    // When kind is absent/defaulted, the event is treated as user-scoped by the indexer.
    const implicitUserEvent = makeEvent({
      event_id: 'ev-user-2',
      label_skill: 'billing',
      kind: undefined as unknown as string,
      status: 'inbox',
    });
    expect(staleViews(implicitUserEvent)).toEqual({ inbox: true, awaiting: false });
  });

  it('a review comment marks the Inbox stale, because the Inbox shows it', () => {
    // Recorded from a live gateway: `escurel:review-comment` arrives `kind: user`,
    // `status: inbox` and appears in `list_inbox` alongside ordinary events — unlike
    // the review TRANSITIONS, which are `kind: system`, `status: processed`.
    const reviewCommentEvent = makeEvent({
      event_id: 'ev-comment-1',
      label_skill: 'escurel:review-comment',
      kind: 'user',
      status: 'inbox',
    });
    expect(staleViews(reviewCommentEvent)).toEqual({ inbox: true, awaiting: false });
  });

  it('an event the queues do not show marks neither', () => {
    // A system event that is not a review transition is hidden from the inbox and
    // changes no count.
    const runnerStatusEvent = makeEvent({
      event_id: 'ev-runner-1',
      label_skill: 'escurel:runner-status',
      kind: 'system',
      status: 'inbox',
    });
    expect(staleViews(runnerStatusEvent)).toEqual({ inbox: false, awaiting: false });

    // Events that have already been processed leave the inbox and require no queue refresh.
    const processedEvent = makeEvent({
      event_id: 'ev-proc-1',
      label_skill: 'customer',
      kind: 'user',
      status: 'processed',
    });
    expect(staleViews(processedEvent)).toEqual({ inbox: false, awaiting: false });
  });
});

describe('LiveRefresher (debounce & coalesce)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('three review events in one tick produce one Awaiting refresh and zero Inbox', () => {
    const inbox = { refresh: vi.fn() };
    const awaiting = { refresh: vi.fn() };
    const refresher = new LiveRefresher({ inbox, awaiting }, 300);

    const ev1 = makeEvent({ event_id: 'e1', label_skill: 'escurel:review', kind: 'system' });
    const ev2 = makeEvent({ event_id: 'e2', label_skill: 'escurel:review', kind: 'system' });
    const ev3 = makeEvent({ event_id: 'e3', label_skill: 'escurel:review', kind: 'system' });

    refresher.handleEvent(ev1);
    refresher.handleEvent(ev2);
    refresher.handleEvent(ev3);

    expect(awaiting.refresh).not.toHaveBeenCalled();
    expect(inbox.refresh).not.toHaveBeenCalled();

    vi.advanceTimersByTime(300);

    expect(awaiting.refresh).toHaveBeenCalledTimes(1);
    expect(inbox.refresh).not.toHaveBeenCalled();

    refresher.dispose();
  });

  it('three ordinary events in one tick produce one Inbox refresh and zero Awaiting', () => {
    const inbox = { refresh: vi.fn() };
    const awaiting = { refresh: vi.fn() };
    const refresher = new LiveRefresher({ inbox, awaiting }, 300);

    const ev1 = makeEvent({
      event_id: 'e1',
      label_skill: 'customer',
      kind: 'user',
      status: 'inbox',
    });
    const ev2 = makeEvent({ event_id: 'e2', label_skill: 'order', kind: 'user', status: 'inbox' });
    const ev3 = makeEvent({
      event_id: 'e3',
      label_skill: 'invoice',
      kind: 'user',
      status: 'inbox',
    });

    refresher.handleEvent(ev1);
    refresher.handleEvent(ev2);
    refresher.handleEvent(ev3);

    expect(inbox.refresh).not.toHaveBeenCalled();
    expect(awaiting.refresh).not.toHaveBeenCalled();

    vi.advanceTimersByTime(300);

    expect(inbox.refresh).toHaveBeenCalledTimes(1);
    expect(awaiting.refresh).not.toHaveBeenCalled();

    refresher.dispose();
  });

  it('coalesces bursts of review and ordinary events to one refresh per view', () => {
    const inbox = { refresh: vi.fn() };
    const awaiting = { refresh: vi.fn() };
    const refresher = new LiveRefresher({ inbox, awaiting }, 300);

    // Multi-draft runs complete with a burst of both review system events and ordinary events.
    refresher.handleEvent(
      makeEvent({ event_id: 'r1', label_skill: 'escurel:review', kind: 'system' }),
    );
    refresher.handleEvent(
      makeEvent({ event_id: 'u1', label_skill: 'customer', kind: 'user', status: 'inbox' }),
    );
    refresher.handleEvent(
      makeEvent({ event_id: 'r2', label_skill: 'escurel:review', kind: 'system' }),
    );
    refresher.handleEvent(
      makeEvent({ event_id: 'u2', label_skill: 'customer', kind: 'user', status: 'inbox' }),
    );
    refresher.handleEvent(
      makeEvent({ event_id: 'r3', label_skill: 'escurel:review', kind: 'system' }),
    );
    refresher.handleEvent(
      makeEvent({ event_id: 'u3', label_skill: 'customer', kind: 'user', status: 'inbox' }),
    );

    expect(inbox.refresh).not.toHaveBeenCalled();
    expect(awaiting.refresh).not.toHaveBeenCalled();

    vi.advanceTimersByTime(300);

    expect(inbox.refresh).toHaveBeenCalledTimes(1);
    expect(awaiting.refresh).toHaveBeenCalledTimes(1);

    refresher.dispose();
  });

  it('events the queues do not show produce zero refreshes', () => {
    const inbox = { refresh: vi.fn() };
    const awaiting = { refresh: vi.fn() };
    const refresher = new LiveRefresher({ inbox, awaiting }, 300);

    refresher.handleEvent(
      makeEvent({ event_id: 's1', label_skill: 'escurel:runner-status', kind: 'system' }),
    );
    refresher.handleEvent(
      makeEvent({ event_id: 'p1', label_skill: 'customer', status: 'processed' }),
    );
    refresher.handleEvent(
      makeEvent({ event_id: 'c1', label_skill: 'escurel:runner-status', kind: 'system' }),
    );

    vi.advanceTimersByTime(300);

    expect(inbox.refresh).not.toHaveBeenCalled();
    expect(awaiting.refresh).not.toHaveBeenCalled();

    refresher.dispose();
  });

  it('resets debounce window on consecutive rapid events and settles once', () => {
    const inbox = { refresh: vi.fn() };
    const awaiting = { refresh: vi.fn() };
    const refresher = new LiveRefresher({ inbox, awaiting }, 300);

    refresher.handleEvent(
      makeEvent({ event_id: 'r1', label_skill: 'escurel:review', kind: 'system' }),
    );
    vi.advanceTimersByTime(150);
    expect(awaiting.refresh).not.toHaveBeenCalled();

    refresher.handleEvent(
      makeEvent({ event_id: 'r2', label_skill: 'escurel:review', kind: 'system' }),
    );
    vi.advanceTimersByTime(150);
    // 300ms from r1, but only 150ms from r2; debounce window was extended.
    expect(awaiting.refresh).not.toHaveBeenCalled();

    vi.advanceTimersByTime(150);
    // Now 300ms since r2; should fire once.
    expect(awaiting.refresh).toHaveBeenCalledTimes(1);

    refresher.dispose();
  });

  it('refreshBoth immediately refreshes both views and cancels pending timers', () => {
    const inbox = { refresh: vi.fn() };
    const awaiting = { refresh: vi.fn() };
    const refresher = new LiveRefresher({ inbox, awaiting }, 300);

    refresher.handleEvent(
      makeEvent({ event_id: 'r1', label_skill: 'escurel:review', kind: 'system' }),
    );
    expect(awaiting.refresh).not.toHaveBeenCalled();

    refresher.refreshBoth();
    expect(inbox.refresh).toHaveBeenCalledTimes(1);
    expect(awaiting.refresh).toHaveBeenCalledTimes(1);

    // Any pending timer was cancelled so no duplicate refresh occurs when it would have expired.
    vi.advanceTimersByTime(300);
    expect(inbox.refresh).toHaveBeenCalledTimes(1);
    expect(awaiting.refresh).toHaveBeenCalledTimes(1);

    refresher.dispose();
  });

  it('dispose cancels any pending debounce timers', () => {
    const inbox = { refresh: vi.fn() };
    const awaiting = { refresh: vi.fn() };
    const refresher = new LiveRefresher({ inbox, awaiting }, 300);

    refresher.handleEvent(
      makeEvent({ event_id: 'r1', label_skill: 'escurel:review', kind: 'system' }),
    );
    refresher.dispose();

    vi.advanceTimersByTime(300);
    expect(awaiting.refresh).not.toHaveBeenCalled();
    expect(inbox.refresh).not.toHaveBeenCalled();
  });
});
