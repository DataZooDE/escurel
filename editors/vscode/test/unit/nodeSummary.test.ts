import { describe, expect, it } from 'vitest';
import { nodeSummary } from '../../src/thread/nodeSummary';

// The Details panel opened with a raw key/value dump (trace_id, harness, attempts ...). A person
// needs one sentence first: what happened, what needs them, what to do next.
describe('nodeSummary', () => {
  it('an open changeset says how many changes wait for review and that it needs the person', () => {
    expect(nodeSummary({ kind: 'changeset', state: 'open', drafts: 2 })).toEqual({
      text: '2 changes are waiting for your review.',
      needsYou: true,
    });
    expect(nodeSummary({ kind: 'changeset', state: 'open', drafts: 1 }).text).toBe(
      '1 change is waiting for your review.',
    );
  });

  it('a decided changeset says what became of it', () => {
    expect(nodeSummary({ kind: 'changeset', state: 'promoted', drafts: 2 })).toEqual({
      text: 'Applied: 2 changes were accepted.',
      needsYou: false,
    });
    expect(nodeSummary({ kind: 'changeset', state: 'discarded', drafts: 2 }).text).toBe(
      'Rejected: nothing was applied.',
    );
  });

  it('runs: working, planned (needs approval), finished, failed, cancelled', () => {
    expect(nodeSummary({ kind: 'run', state: 'running' })).toEqual({
      text: 'The agent is working on it.',
      needsYou: false,
    });
    expect(nodeSummary({ kind: 'run', state: 'planned' })).toEqual({
      text: 'The agent made a plan and is waiting for you to approve it.',
      needsYou: true,
    });
    expect(nodeSummary({ kind: 'run', state: 'processed', duration: '6 s' }).text).toBe(
      'The agent finished in 6 s.',
    );
    expect(nodeSummary({ kind: 'run', state: 'processed' }).text).toBe('The agent finished.');
    expect(
      nodeSummary({ kind: 'run', state: 'dead_letter', reason: 'harness not allowed' }),
    ).toEqual({
      text: 'The agent stopped and could not finish: harness not allowed. Retry it, or ask an admin.',
      needsYou: true,
    });
    expect(nodeSummary({ kind: 'run', state: 'failed' }).needsYou).toBe(true);
    expect(nodeSummary({ kind: 'run', state: 'cancelled' }).text).toBe('This run was cancelled.');
  });

  it('events: waiting, handled with the runs that followed', () => {
    expect(nodeSummary({ kind: 'event', state: 'inbox' })).toEqual({
      text: 'A signal arrived and has not been handled yet.',
      needsYou: false,
    });
    expect(nodeSummary({ kind: 'event', state: 'processed', runs: 2 }).text).toBe(
      'This signal was handled. 2 agent runs followed.',
    );
    expect(nodeSummary({ kind: 'event', state: 'processed', runs: 0 }).text).toBe(
      'This signal was handled.',
    );
  });

  it('drafts: a proposed change to a page, and its outcome', () => {
    expect(nodeSummary({ kind: 'draft', state: 'open', target: 'order-4500131' })).toEqual({
      text: 'A proposed change to order-4500131 is waiting for review.',
      needsYou: true,
    });
    expect(nodeSummary({ kind: 'draft', state: 'promoted', target: 'order-4500131' }).text).toBe(
      'The change to order-4500131 was applied.',
    );
  });

  it('says nothing wrong about a state it does not know', () => {
    expect(nodeSummary({ kind: 'run', state: 'something_new' }).needsYou).toBe(false);
    expect(nodeSummary({ kind: 'event' }).text.length > 0).toBe(true);
  });
});
