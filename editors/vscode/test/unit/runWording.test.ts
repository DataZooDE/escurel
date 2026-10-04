import { describe, expect, it } from 'vitest';
import { emptyAttempts, emptyPlan, runByline, statusIconName } from '../../src/runs/runWording';

// 'Harness echo · Autonomy review · Depth 0' is how the data is named, not how a person talks.
describe('runByline', () => {
  it('says who ran it and what that means for changes, in a sentence', () => {
    expect(runByline({ harness: 'echo', autonomy: 'review', depth: 0 })).toBe(
      'Run by the echo agent. Its changes need your approval.',
    );
    expect(runByline({ harness: 'claude', model: 'opus', autonomy: 'auto', depth: 0 })).toBe(
      'Run by the claude agent (opus). Its changes are applied without review.',
    );
  });
  it('mentions the follow-up level only when it is one', () => {
    expect(runByline({ harness: 'echo', autonomy: 'review', depth: 2 })).toBe(
      'Run by the echo agent. Its changes need your approval. Follow-up level 2.',
    );
  });
  it('says nothing it does not know', () => {
    expect(runByline({})).toBe('');
    expect(runByline({ autonomy: 'review' })).toBe('Its changes need your approval.');
  });
});

describe('empty states', () => {
  it('a run that has only just started says it is starting, not that nothing was reported', () => {
    expect(emptyAttempts('running')).toBe('Starting…');
    expect(emptyPlan('running')).toBe('The agent has not reported a plan yet.');
    expect(emptyAttempts('planned')).toBe('Starting…');
  });
  it('a finished run says nothing was recorded', () => {
    expect(emptyAttempts('processed')).toBe('No attempts were recorded.');
    expect(emptyPlan('failed')).toBe('No plan was recorded.');
  });
});

describe('statusIconName', () => {
  it('gives every state a SHAPE of its own, so colour is never the only cue', () => {
    expect(statusIconName('processed')).toBe('check');
    expect(statusIconName('running')).toBe('sync');
    expect(statusIconName('failed')).toBe('cross');
    expect(statusIconName('dead_letter')).toBe('cross');
    expect(statusIconName('cancelled')).toBe('warn');
    expect(statusIconName('planned')).toBe('warn');
    expect(statusIconName('mystery')).toBe('warn');
  });
});

import { statusWord } from '../../src/runs/runWording';
describe('statusWord', () => {
  it('uses the words a person uses for a run state', () => {
    expect(statusWord('processed')).toBe('done');
    expect(statusWord('dead_letter')).toBe('gave up');
    expect(statusWord('planned')).toBe('plan ready');
    expect(statusWord('running')).toBe('running');
    expect(statusWord('in_between')).toBe('in between');
  });
});
