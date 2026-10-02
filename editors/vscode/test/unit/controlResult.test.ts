import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client/types';
import {
  describeOutcome,
  findControlResult,
  matchResult,
  parseControlResult,
} from '../../src/runs/controlResult';
import recorded from './fixtures/runner/run-control-results.json';

const events = recorded.events as Event[];

describe('recorded runner control results', () => {
  it('parses and describes every recorded outcome', () => {
    expect(events.map((event) => describeOutcome(parseControlResult(event)!))).toEqual([
      'Dispatch resumed.',
      'Dispatch paused.',
      'Requeued; a new run has started.',
      'That run is not running any more (run is dead_letter).',
    ]);
  });

  it('matches the request event id in provenance even when run_id is null', () => {
    expect(
      matchResult(events, { eventId: '01M3Y7NNG4233H9BB05Z6SF0JY', action: 'pause' })?.outcome,
    ).toBe('paused');
    expect(
      matchResult(events, {
        eventId: '01M3Y7N123XCWQ1J2B08SCEWSX',
        action: 'cancel',
        runId: 'wrong',
      }),
    ).toBeUndefined();
    expect(matchResult(events, { eventId: 'missing', action: 'resume' })).toBeUndefined();
  });

  it('shows unrecognized outcomes verbatim and rejects malformed bodies', () => {
    expect(describeOutcome({ action: 'cancel', outcome: 'cancelled', runId: 'run' })).toBe(
      'Run cancelled.',
    );
    expect(describeOutcome({ action: 'cancel', outcome: 'future_outcome', runId: null })).toBe(
      'future_outcome',
    );
    expect(parseControlResult({ ...events[0]!, body: '{' })).toBeUndefined();
  });
});

describe('a refusal says why', () => {
  // The runner answers `outcome: "refused"` with the reason in `detail` (runner main.rs,
  // ControlOutcome::refused): "that run is not retriable because ...". Showing only the word
  // would leave the user unable to tell what to do about it.
  it('includes the runner’s reason', () => {
    expect(
      describeOutcome({
        action: 'retry',
        outcome: 'refused',
        detail: 'run is still running',
        runId: 'r',
      }),
    ).toBe('The runner refused: run is still running.');
  });

  it('still says it was refused when no reason came with it', () => {
    expect(describeOutcome({ action: 'retry', outcome: 'refused', runId: 'r' })).toBe(
      'The runner refused that request.',
    );
  });
});

describe('findControlResult', () => {
  const answer = (id: string, action = 'pause') =>
    ({
      event_id: `res-${id}`,
      label_skill: 'escurel:run-control-result',
      title: action,
      body: JSON.stringify({ action, outcome: 'paused', detail: null, run_id: null }),
      provenance: { control: { request_event_id: id } },
    }) as never;
  const pages = (all: unknown[][]) => {
    let calls = 0;
    const fetchPage = async (cursor?: string) => {
      const i = cursor ? Number(cursor) : 0;
      calls += 1;
      return {
        events: all[i] as never[],
        ...(i + 1 < all.length ? { next_cursor: String(i + 1) } : {}),
      };
    };
    return { fetchPage, calls: () => calls };
  };

  it('finds the answer on a later page, not only the newest', async () => {
    const { fetchPage } = pages([[answer('a')], [answer('b')], [answer('want')]]);
    const found = await findControlResult(fetchPage, { eventId: 'want', action: 'pause' });
    expect(found?.outcome).toBe('paused');
  });

  it('stops asking after a bounded number of pages', async () => {
    const { fetchPage, calls } = pages(Array.from({ length: 20 }, (_, i) => [answer(`x${i}`)]));
    expect(
      await findControlResult(fetchPage, { eventId: 'missing', action: 'pause' }, 3),
    ).toBeUndefined();
    expect(calls()).toBe(3);
  });
});

describe('describeOutcome: retry and requeue', () => {
  it('says what the PERSON did (retry vs requeue) and keeps run ids out of the sentence', () => {
    const requeued = { outcome: 'requeued', runId: null, newRunId: '01M3Y7NK030SE8TVFA6XJ097NH' };
    expect(describeOutcome({ action: 'retry', ...requeued })).toBe(
      'Retried; a new run has started.',
    );
    expect(describeOutcome({ action: 'requeue', ...requeued })).toBe(
      'Requeued; a new run has started.',
    );
    expect(describeOutcome({ action: 'retry', outcome: 'requeued', runId: null })).toBe('Retried.');
  });
});
