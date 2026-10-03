import { describe, expect, it } from 'vitest';
import { acceptDetailsAction } from '../../src/thread/detailsRouting';

const shown = {
  rootEventId: 'root-A',
  nodeId: 'run-1',
  pageId: 'markdown/instances/customer-order__order-1.md',
  skills: ['supplier-risk'],
};
const open = (ids: string[]) => (id: string) => ids.includes(id);
const action = {
  type: 'details-action',
  rootEventId: 'root-A',
  message: { type: 'run-control', action: 'retry', runId: 'run-1' },
};

// The details view is a webview of its own: it can post anything. The host acts only for the
// thread whose node it is showing, and only for the three actions an inspector has.
describe('acceptDetailsAction', () => {
  it('accepts an inspector action for the thread being shown, and returns the inner message', () => {
    expect(acceptDetailsAction(shown, open(['root-A']), action)).toEqual({
      rootEventId: 'root-A',
      message: action.message,
    });
  });

  it('refuses an action for a thread that is not the one shown (a forged or stale id)', () => {
    expect(
      acceptDetailsAction(shown, open(['root-A', 'root-B']), { ...action, rootEventId: 'root-B' }),
    ).toBeUndefined();
  });

  it('refuses when nothing is shown, or the shown thread is no longer open', () => {
    expect(acceptDetailsAction(undefined, open(['root-A']), action)).toBeUndefined();
    expect(acceptDetailsAction(shown, open([]), action)).toBeUndefined();
  });

  it('refuses anything that is not one of the three inspector actions', () => {
    for (const type of [
      'promote',
      'discard',
      'toggle-collapse',
      'expand-all',
      'select-node',
      'open-node',
    ]) {
      expect(
        acceptDetailsAction(shown, open(['root-A']), { ...action, message: { type } }),
      ).toBeUndefined();
    }
  });

  it('refuses malformed messages', () => {
    for (const bad of [
      undefined,
      null,
      'x',
      7,
      [],
      {},
      { type: 'details-action' },
      { type: 'details-action', rootEventId: 3, message: {} },
      { type: 'details-action', rootEventId: 'root-A' },
      { type: 'details-action', rootEventId: 'root-A', message: 'run-control' },
    ]) {
      expect(acceptDetailsAction(shown, open(['root-A']), bad)).toBeUndefined();
    }
  });
});

// The details view shows ONE node. Even the thread's own validation would allow another run or page
// of the same thread, so the host also holds the view to the node it is showing.
describe('acceptDetailsAction: only the node being shown', () => {
  const ok = (message: unknown) =>
    acceptDetailsAction(shown, open(['root-A']), {
      type: 'details-action',
      rootEventId: 'root-A',
      message,
    });

  it('accepts a run control for the shown run, refuses one for any other run', () => {
    expect(ok({ type: 'run-control', action: 'retry', runId: 'run-1' })).toBeDefined();
    expect(ok({ type: 'run-control', action: 'retry', runId: 'run-2' })).toBeUndefined();
    expect(ok({ type: 'run-control', action: 'requeue', eventId: 'ev-9' })).toBeUndefined();
    expect(ok({ type: 'run-control', action: 'retry' })).toBeUndefined();
  });

  it('accepts a start for the shown page and an offered skill, refuses another page or skill', () => {
    const base = { type: 'start-skill', mode: 'background' };
    expect(ok({ ...base, skill: 'supplier-risk', pageId: shown.pageId })).toBeDefined();
    expect(
      ok({ ...base, skill: 'supplier-risk', pageId: 'markdown/instances/other.md' }),
    ).toBeUndefined();
    expect(ok({ ...base, skill: 'delete-everything', pageId: shown.pageId })).toBeUndefined();
  });

  it('accepts viewing an offered skill only', () => {
    expect(ok({ type: 'view-skill', skill: 'supplier-risk' })).toBeDefined();
    expect(ok({ type: 'view-skill', skill: 'something-else' })).toBeUndefined();
  });
});
