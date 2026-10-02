import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client/types';
import { describeOutcome, matchResult, parseControlResult } from '../../src/runs/controlResult';
import recorded from './fixtures/runner/run-control-results.json';

const events = recorded.events as Event[];

describe('recorded runner control results', () => {
  it('parses and describes every recorded outcome', () => {
    expect(events.map((event) => describeOutcome(parseControlResult(event)!))).toEqual([
      'Dispatch resumed.',
      'Dispatch paused.',
      'Requeued; new run 01M3Y7NK030SE8TVFA6XJ097NH.',
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
