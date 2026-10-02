import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client';
import { controlRequest } from '../../src/runs/controlCommands';
import { buildRunnerRows, extractDeadLetters } from '../../src/views/runnerModel';
import runnerStatus from './fixtures/runner/runner-status-newest3.json';
import runRows from './fixtures/runner/run-rows-newest30.json';

// The Runner view's context menu hands a command the tree ROW. These are real rows, built from the
// recordings, passed to the real argument parser: the menu works only if the ids a row carries are
// the ids the command reads.
const status = (runnerStatus as { events: Event[] }).events[0]!;
const deadLetters = extractDeadLetters((runRows as { events: Event[] }).events);

function rowsOf(group: 'deadLetters' | 'liveRuns') {
  const live = {
    ...status,
    body: JSON.stringify({
      ...(JSON.parse(status.body as string) as object),
      live_runs: [
        {
          run_id: '01RUNLIVE',
          event_id: '01EVLIVE',
          instance_page_id: 'markdown/instances/x__y.md',
        },
      ],
    }),
  } as Event;
  const rows = buildRunnerRows(live, deadLetters, { admin: 'admin' });
  return rows.find((r) => r.kind === group)!.children!;
}

describe('controlRequest with the Runner view’s rows', () => {
  it('requeues the TRIGGER event of a dead letter, not the run', () => {
    const row = rowsOf('deadLetters')[0]!;
    expect(row.eventId).toBe('01M3Y7MD5FYFSDTTEM0WM64BCV');
    expect(controlRequest('requeue', row)).toEqual({
      action: 'requeue',
      eventId: '01M3Y7MD5FYFSDTTEM0WM64BCV',
    });
  });

  it('retries a dead letter by its run', () => {
    expect(controlRequest('retry', rowsOf('deadLetters')[0]!)).toEqual({
      action: 'retry',
      runId: '01M3Y7MD5XWKPB08508JGNBW6M',
    });
  });

  it('cancels a live run by its run id', () => {
    expect(controlRequest('cancel', rowsOf('liveRuns')[0]!)).toEqual({
      action: 'cancel',
      runId: '01RUNLIVE',
    });
  });

  it('still takes a bare id, and pause/resume take nothing', () => {
    expect(controlRequest('cancel', '01R')).toEqual({ action: 'cancel', runId: '01R' });
    expect(controlRequest('requeue', '01E')).toEqual({ action: 'requeue', eventId: '01E' });
    expect(controlRequest('pause', undefined)).toEqual({ action: 'pause' });
  });
});
