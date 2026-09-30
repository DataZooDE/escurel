import { describe, expect, it } from 'vitest';
import { commandForTarget, rootEventIdOf } from '../../src/thread/nodeTarget';

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
