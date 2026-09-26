import { describe, expect, it, vi } from 'vitest';
import { LiveViewSocket } from '../../src/liveView';
import type { Services } from '../../src/services';

/**
 * A stand-in for the per-window wiring. Only the three things the socket reads.
 */
function servicesStub(): Services {
  return {
    gatewayUrl: 'http://127.0.0.1:1',
    auth: { refresher: { get: async () => undefined } },
    onDidChange: () => ({ dispose: () => undefined }),
  } as unknown as Services;
}

describe('LiveViewSocket', () => {
  it('refuses a filter that is not lineage-scoped', () => {
    // Only `root_event_id` and `run_id` make a subscription lineage-scoped, and
    // only a lineage-scoped one resumes by log position and replays every status.
    // Anything else resumes inbox-only, so a thread built on it would silently
    // miss the run events it exists to show. Refusing beats degrading.
    expect(
      () =>
        new LiveViewSocket(
          servicesStub(),
          { label_skill: 'escurel:run' } as unknown as { run_id: string },
          () => undefined,
          () => undefined,
        ),
    ).toThrow(/root_event_id or run_id/);
  });

  it('accepts a thread scope and a run scope', () => {
    const made: LiveViewSocket[] = [];
    for (const filters of [{ root_event_id: 'ev-1' }, { run_id: 'run-1' }] as const) {
      const s = new LiveViewSocket(servicesStub(), filters, vi.fn(), vi.fn());
      made.push(s);
    }
    expect(made).toHaveLength(2);
    for (const s of made) s.dispose();
  });
});
