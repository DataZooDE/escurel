import type { ThreadNode } from '../../src/shared/protocol';
import { describe, expect, it } from 'vitest';
import { commandForTarget, resolveGate, rootEventIdOf } from '../../src/thread/nodeTarget';

describe('commandForTarget', () => {
  it('routes each node target to the command that already owns that surface', () => {
    expect(commandForTarget({ open: 'thread', rootEventId: 'e1' })).toEqual({
      command: 'escurel.openThread',
      args: ['e1'],
    });
    expect(commandForTarget({ open: 'run', runId: 'r1' })).toEqual({
      command: 'escurel.openRun',
      args: ['r1'],
    });
    expect(commandForTarget({ open: 'page', pageId: 'markdown/instances/a__b.md' })).toEqual({
      command: 'escurel.openInstance',
      args: ['markdown/instances/a__b.md'],
    });
  });

  it('opens review with the shape resolveReviewTarget reads', () => {
    // A draft carries its changeset id too; the review command must be told which one this
    // node is, or it opens the changeset picker instead of the draft (the M2 defect).
    expect(commandForTarget({ open: 'review', draftId: 'd1' })).toEqual({
      command: 'escurel.openReview',
      args: [{ draftId: 'd1' }],
    });
    expect(commandForTarget({ open: 'review', changesetId: 'c1' })).toEqual({
      command: 'escurel.openReview',
      args: [{ changesetId: 'c1' }],
    });
  });

  it('does nothing for a node with nothing to open', () => {
    expect(commandForTarget({ open: 'nothing' })).toBeUndefined();
  });
});

describe('rootEventIdOf', () => {
  it('reads a bare id, an inbox event, and refuses everything else', () => {
    expect(rootEventIdOf('e1')).toBe('e1');
    expect(rootEventIdOf({ root_event_id: 'root', event_id: 'e2' })).toBe('root');
    // A root event has no root_event_id of its own: its id IS the root.
    expect(rootEventIdOf({ root_event_id: null, event_id: 'e3' })).toBe('e3');
    expect(rootEventIdOf(undefined)).toBeUndefined();
    expect(rootEventIdOf({})).toBeUndefined();
    expect(rootEventIdOf('')).toBeUndefined();
  });
});

describe('resolveGate', () => {
  // A message from a webview is input, however well-behaved the webview is: it is checked
  // against the thread the HOST loaded before it can reach a command that writes.
  const node = (over: Record<string, unknown>) =>
    ({
      id: 'x',
      kind: 'draft',
      parent: null,
      children: [],
      state: 'open',
      tone: 'instance',
      title: 't',
      meta: [],
      chips: [],
      target: { open: 'nothing' },
      collapsible: false,
      ...over,
    }) as ThreadNode;
  const view = {
    rootEventId: 'r',
    columns: [],
    loadingMore: false,
    nodes: [
      node({ id: 'cs1', kind: 'changeset', gate: { drafts: 2, changesetId: 'cs1' } }),
      node({ id: 'd1', kind: 'draft', gate: { drafts: 1, draftId: 'd1' } }),
      // Decided: no gate, so nothing on the canvas offers it and nothing may act on it.
      node({ id: 'd2', kind: 'draft', state: 'promoted' }),
    ],
  };

  it('passes a changeset or a draft that carries an open gate', () => {
    expect(resolveGate(view, { changesetId: 'cs1' })).toEqual({ changesetId: 'cs1' });
    expect(resolveGate(view, { draftId: 'd1' })).toEqual({ draftId: 'd1' });
  });

  it('refuses an id the loaded thread does not hold', () => {
    expect(resolveGate(view, { draftId: 'someone-elses-draft' })).toBeUndefined();
    expect(resolveGate(view, { changesetId: 'unrelated' })).toBeUndefined();
  });

  it('refuses a decided draft, which has no gate', () => {
    expect(resolveGate(view, { draftId: 'd2' })).toBeUndefined();
  });

  it('refuses a message naming both ids, or neither', () => {
    // Both would have selected the changeset silently; the sender must say which.
    expect(resolveGate(view, { changesetId: 'cs1', draftId: 'd1' })).toBeUndefined();
    expect(resolveGate(view, {})).toBeUndefined();
  });

  it('refuses an id that names a node of the other kind', () => {
    expect(resolveGate(view, { draftId: 'cs1' })).toBeUndefined();
    expect(resolveGate(view, { changesetId: 'd1' })).toBeUndefined();
  });
});
