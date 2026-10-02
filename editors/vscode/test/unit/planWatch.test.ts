import { describe, expect, it } from 'vitest';
import type { LineageNode } from '../../src/client/types';
import { watchPlan } from '../../src/start/planWatch';

describe('watchPlan', () => {
  it('returns planned immediately when a run node is already planned', async () => {
    const runNode: LineageNode = {
      id: '01RUN1',
      type: 'run',
      state: 'planned',
      parent: '01EVT1',
    };

    const res = await watchPlan({
      rootEventId: '01EVT1',
      fetchLineage: async () => ({ nodes: [runNode] }),
      sleep: async () => {},
    });

    expect(res).toEqual({
      state: 'planned',
      runId: '01RUN1',
      node: runNode,
    });
  });

  it('polls through waiting states until run reaches planned', async () => {
    let tick = 0;
    const sleeps: number[] = [];
    const eventNode: LineageNode = {
      id: '01EVT1',
      type: 'event',
      state: 'inbox',
      parent: null,
    };
    const runningNode: LineageNode = {
      id: '01RUN1',
      type: 'run',
      state: 'running',
      parent: '01EVT1',
    };
    const plannedNode: LineageNode = {
      id: '01RUN1',
      type: 'run',
      state: 'planned',
      parent: '01EVT1',
    };

    const res = await watchPlan({
      rootEventId: '01EVT1',
      fetchLineage: async () => {
        tick++;
        if (tick === 1) return { nodes: [eventNode] }; // no run yet
        if (tick === 2) return { nodes: [eventNode, runningNode] }; // running
        return { nodes: [eventNode, plannedNode] }; // planned
      },
      sleep: async (ms) => {
        sleeps.push(ms);
      },
      pollIntervalMs: 750,
    });

    expect(res).toEqual({
      state: 'planned',
      runId: '01RUN1',
      node: plannedNode,
    });
    expect(sleeps).toEqual([750, 750]);
  });

  it('detects a run that failed and reports the reason', async () => {
    const failedNode: LineageNode = {
      id: '01RUN_FAIL',
      type: 'run',
      state: 'failed',
      summary: 'harness "unknown" is not in allow-list',
      parent: '01EVT1',
    };

    const res = await watchPlan({
      rootEventId: '01EVT1',
      fetchLineage: async () => ({ nodes: [failedNode] }),
      sleep: async () => {},
    });

    expect(res.state).toBe('failed');
    if (res.state === 'failed') {
      expect(res.runId).toBe('01RUN_FAIL');
      expect(res.reason).toBe('harness "unknown" is not in allow-list');
      expect(res.node).toEqual(failedNode);
    }
  });

  it('stops at once when the plan run is cancelled, instead of polling to the timeout', async () => {
    let polls = 0;
    const res = await watchPlan({
      rootEventId: '01EVT1',
      fetchLineage: async () => {
        polls += 1;
        return { nodes: [{ id: '01RUN_C', type: 'run', state: 'cancelled', parent: '01EVT1' }] };
      },
      sleep: async () => {},
    });
    expect(res.state).toBe('failed');
    if (res.state === 'failed') expect(res.reason).toMatch(/cancel/i);
    expect(polls).toBe(1);
  });

  it('detects a dead_letter run and reports its reason', async () => {
    const deadLetterNode: LineageNode = {
      id: '01RUN_DEAD',
      type: 'run',
      state: 'dead_letter',
      reason: 'RetriesExhausted',
      parent: '01EVT1',
    };

    const res = await watchPlan({
      rootEventId: '01EVT1',
      fetchLineage: async () => ({ nodes: [deadLetterNode] }),
      sleep: async () => {},
    });

    expect(res.state).toBe('failed');
    if (res.state === 'failed') {
      expect(res.runId).toBe('01RUN_DEAD');
      expect(res.reason).toBe('RetriesExhausted');
    }
  });

  it('times out after timeoutMs using a fake clock', async () => {
    let currentTime = 1_000_000;
    const runningNode: LineageNode = {
      id: '01RUN_STUCK',
      type: 'run',
      state: 'running',
      parent: '01EVT1',
    };

    const res = await watchPlan({
      rootEventId: '01EVT1',
      fetchLineage: async () => ({ nodes: [runningNode] }),
      pollIntervalMs: 750,
      timeoutMs: 3000,
      now: () => currentTime,
      sleep: async (ms) => {
        currentTime += ms;
      },
    });

    expect(res).toEqual({ state: 'timeout' });
    expect(currentTime).toBeGreaterThanOrEqual(1_003_000);
  });

  it('aborts when cancelled before first poll', async () => {
    let fetched = false;
    const res = await watchPlan({
      rootEventId: '01EVT1',
      fetchLineage: async () => {
        fetched = true;
        return { nodes: [] };
      },
      isCancelled: () => true,
      sleep: async () => {},
    });

    expect(res).toEqual({ state: 'cancelled' });
    expect(fetched).toBe(false);
  });

  it('aborts when cancelled while waiting between polls', async () => {
    let pollCount = 0;
    let cancelled = false;

    const res = await watchPlan({
      rootEventId: '01EVT1',
      fetchLineage: async () => {
        pollCount++;
        return { nodes: [] };
      },
      isCancelled: () => cancelled,
      sleep: async () => {
        cancelled = true; // cancelled during sleep
      },
      pollIntervalMs: 750,
    });

    expect(res).toEqual({ state: 'cancelled' });
    expect(pollCount).toBe(1);
  });
});
