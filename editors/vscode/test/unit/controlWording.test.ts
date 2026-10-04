import { describe, expect, it } from 'vitest';
import {
  asksHere,
  confirmationFor,
  outcomeChannel,
  progressTitle,
} from '../../src/runs/controlWording';

// Routine outcomes belong in the status bar (one line, expires); a toast is for what needs the user.
describe('outcomeChannel', () => {
  it('sends the outcomes that went as planned to the status bar', () => {
    for (const outcome of ['cancelled', 'requeued', 'paused', 'resumed'])
      expect(outcomeChannel(outcome)).toBe('status');
  });
  it('keeps a toast for what did not go as planned', () => {
    for (const outcome of ['refused', 'not_live', 'something-new'])
      expect(outcomeChannel(outcome)).toBe('toast');
  });
});

describe('confirmationFor', () => {
  it('asks before cancelling, and says what is kept', () => {
    const c = confirmationFor('cancel')!;
    expect(c.message).toBe('Cancel this run?');
    expect(c.detail).toContain('Work already done is kept');
    expect(c.button).toBe('Cancel run');
  });
  it('does not ask for the actions that are undoable or self-describing', () => {
    expect(confirmationFor('retry')).toBeUndefined();
    expect(confirmationFor('resume')).toBeUndefined();
  });
});

describe('progressTitle', () => {
  it('describes what a retry does', () => {
    expect(progressTitle('retry')).toBe(
      'Retrying: starts a new run, this attempt stays in history',
    );
  });
  it('is one short line for the others', () => {
    expect(progressTitle('cancel')).toBe('Cancelling…');
    expect(progressTitle('pause')).toBe('Pausing agents…');
  });
});

describe('asksHere', () => {
  it('asks for a tree row, and never for a webview or a programmatic call', () => {
    expect(asksHere({ kind: 'run', runId: 'r1' })).toBe(true);
    expect(asksHere({ kind: 'dispatch' })).toBe(true);
    // A webview posts { runId } after asking inline; a bare id comes from code.
    expect(asksHere({ runId: 'r1' })).toBe(false);
    expect(asksHere('r1')).toBe(false);
    expect(asksHere(undefined)).toBe(false);
  });
});
