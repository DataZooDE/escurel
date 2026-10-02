import { describe, expect, it } from 'vitest';
import { acceptLoadMore, traceIdToCopy } from '../../src/runs/runActions';
import type { RunView } from '../../src/shared/protocol';

const view = { runId: 'r', traceId: 'a1b2c3', nextAfter: 50, calls: [] } as unknown as RunView;

// Two run-panel messages carried a value the host then trusted: a trace id to put on the clipboard
// and a paging cursor. A forged message could plant an arbitrary string for the user to paste, or
// page somewhere the host never offered.
describe('copying the trace id', () => {
  it('copies the host’s own trace id, whatever the webview says', () => {
    expect(traceIdToCopy(view)).toBe('a1b2c3');
  });

  it('copies nothing when the run has none', () => {
    expect(traceIdToCopy({ ...view, traceId: undefined })).toBeUndefined();
    expect(traceIdToCopy(undefined)).toBeUndefined();
  });
});

describe('loading more tool calls', () => {
  it('accepts exactly the cursor the host offered', () => {
    expect(acceptLoadMore(view, 50)).toBe(true);
  });

  it('refuses any other cursor, and a non-integer', () => {
    for (const after of [0, 49, 51, -1, 1.5, '50' as never, undefined as never]) {
      expect(acceptLoadMore(view, after)).toBe(false);
    }
  });

  it('refuses when there is no next page', () => {
    expect(acceptLoadMore({ ...view, nextAfter: null }, 50)).toBe(false);
  });
});
