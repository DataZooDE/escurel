import { describe, expect, it } from 'vitest';
import { displayStepStatus, runHeading, runTabTitle } from '../../src/runs/runTitle';

// Run detail opened as "Run 01M3ZMF45C3G2SYSGE003N344Z" in the page and "Run · 3N344Z" in the tab: an
// id nobody can tell from the next. What a person knows is which skill ran on which page.
describe('runHeading', () => {
  it('names the skill and the page it ran on, not the id', () => {
    expect(
      runHeading({
        runId: '01M3ZMF45C3G2SYSGE003N344Z',
        skill: 'supplier-risk',
        targetPageId: 'markdown/instances/customer-order__order-4500123.md',
      }),
    ).toEqual({ title: 'supplier-risk on order-4500123', id: '01M3ZMF45C3G2SYSGE003N344Z' });
  });

  it('falls back to the skill alone, then to the short id', () => {
    expect(runHeading({ runId: '01ABCDEF', skill: 'supplier-risk' }).title).toBe('supplier-risk');
    expect(runHeading({ runId: '01M3ZMF45C3G2SYSGE003N344Z' }).title).toBe('Run 3N344Z');
  });
});

describe('runTabTitle', () => {
  it('is short, readable and distinct per run', () => {
    expect(
      runTabTitle({
        runId: '01M3ZMF45C3G2SYSGE003N344Z',
        skill: 'supplier-risk',
        targetPageId: 'markdown/instances/customer-order__order-4500123.md',
      }),
    ).toBe('Run · supplier-risk · order-4500123');
    expect(runTabTitle({ runId: '01M3ZMF45C3G2SYSGE003N344Z' })).toBe('Run · 3N344Z');
  });
});

// A run that has finished cannot still be doing a step. The plan said "in progress" under a
// "processed" badge, and a reader cannot tell which to believe.
describe('displayStepStatus', () => {
  it('keeps what the plan says while the run is live or planned', () => {
    expect(displayStepStatus('in_progress', 'running')).toBe('in_progress');
    expect(displayStepStatus('pending', 'planned')).toBe('pending');
  });

  it('shows a step that was in progress when the run ended as not finished', () => {
    for (const status of ['processed', 'failed', 'dead_letter', 'cancelled']) {
      expect(displayStepStatus('in_progress', status)).toBe('unfinished');
    }
  });

  it('leaves completed, blocked and pending steps alone', () => {
    expect(displayStepStatus('completed', 'processed')).toBe('completed');
    expect(displayStepStatus('blocked', 'failed')).toBe('blocked');
    expect(displayStepStatus('pending', 'processed')).toBe('pending');
  });
});
