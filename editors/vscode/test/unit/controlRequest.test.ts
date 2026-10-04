import { describe, expect, it } from 'vitest';
import type { Event } from '../../src/client';
import { controlRequest } from '../../src/runs/controlCommands';
import { buildRunsTree, foldRuns, type RunsNode } from '../../src/views/runsModel';
import runRows from './fixtures/runner/run-rows-newest30.json';

// The Runner view's context menu hands a command the tree ROW. These are real rows, built from the
// recordings, passed to the real argument parser: the menu works only if the ids a row carries are
// the ids the command reads.
const NOW = Date.parse('2026-10-02T12:00:00Z');
const recorded = (runRows as { events: Event[] }).events;

function rowsOf(group: 'group:attention' | 'group:running'): RunsNode[] {
  const live = {
    event_id: 'run:01RUNLIVE:started',
    at: '2026-10-02T11:59:00Z',
    source: 'escurel-runner',
    mime: 'application/json',
    label_skill: 'escurel:run',
    instance_page_id: 'markdown/instances/x__y.md',
    status: 'processed',
    title: 'run-started',
    body: '{}',
    provenance: { runner: { event_id: '01EVLIVE' } },
    kind: 'system',
    root_event_id: '01ROOT',
    run_id: '01RUNLIVE',
  } as Event;
  const records = foldRuns([live, ...recorded], { nowMs: NOW, liveRunIds: new Set(['01RUNLIVE']) });
  const tree = buildRunsTree({
    records,
    filter: {},
    nowMs: NOW,
    historyLimit: 25,
    hasMoreHistory: false,
    runner: undefined,
    isAdmin: true,
  });
  return tree.find((n) => n.id === group)!.children!;
}

describe('controlRequest with the Runner view’s rows', () => {
  it('requeues the TRIGGER event of a dead letter, not the run', () => {
    const row = rowsOf('group:attention')[0]!;
    expect(row.eventId).toBe('01M3Y7MD5FYFSDTTEM0WM64BCV');
    expect(controlRequest('requeue', row)).toEqual({
      action: 'requeue',
      eventId: '01M3Y7MD5FYFSDTTEM0WM64BCV',
    });
  });

  it('retries a dead letter by its run', () => {
    expect(controlRequest('retry', rowsOf('group:attention')[0]!)).toEqual({
      action: 'retry',
      runId: '01M3Y7MD5XWKPB08508JGNBW6M',
    });
  });

  it('cancels a live run by its run id', () => {
    expect(controlRequest('cancel', rowsOf('group:running')[0]!)).toEqual({
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
