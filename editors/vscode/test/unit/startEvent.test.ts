import { describe, expect, it } from 'vitest';
import { buildApprovalEvent, buildStartEvent, manualModeFor } from '../../src/start/startEvent';

const PAGE = 'markdown/instances/customer-order__order-4500123.md';

// The contract is the gateway's (tools_write.rs, workbench P2-5): `provenance.manual` is
// `{harness?, mode?, approved_plan_run_id?}`, `mode` is run|plan, and `requested_by` is stamped
// from the token, so it is never sent.
describe('buildStartEvent', () => {
  it('captures a user event for the skill on the instance, marked as a manual start', () => {
    const e = buildStartEvent({ skill: 'supplier-risk', pageId: PAGE, mode: 'run' });
    expect(e.label_skill).toBe('supplier-risk');
    expect(e.instance_page_id).toBe(PAGE);
    expect(e.source).toBe('workbench');
    expect(e.provenance).toEqual({ manual: { mode: 'run' } });
  });

  it('asks for a plan when asked to', () => {
    expect(buildStartEvent({ skill: 's', pageId: PAGE, mode: 'plan' }).provenance).toEqual({
      manual: { mode: 'plan' },
    });
  });

  it('sends a harness only when one was chosen', () => {
    const none = buildStartEvent({ skill: 's', pageId: PAGE, mode: 'run' });
    expect(JSON.stringify(none.provenance)).not.toContain('harness');
    for (const blank of ['', '   ', undefined]) {
      expect(
        JSON.stringify(buildStartEvent({ skill: 's', pageId: PAGE, mode: 'run', harness: blank })),
      ).not.toContain('harness');
    }
    expect(
      buildStartEvent({ skill: 's', pageId: PAGE, mode: 'run', harness: ' claude ' }).provenance,
    ).toEqual({ manual: { mode: 'run', harness: 'claude' } });
  });

  it('never claims who asked: requested_by is the gateway’s to stamp', () => {
    expect(
      JSON.stringify(buildStartEvent({ skill: 's', pageId: PAGE, mode: 'run' })),
    ).not.toContain('requested_by');
  });

  it('gives the event a title that names the skill and the instance, not a ULID', () => {
    const e = buildStartEvent({ skill: 'supplier-risk', pageId: PAGE, mode: 'run' });
    expect(e.title).toBe('supplier-risk · order-4500123');
  });
});

describe('buildApprovalEvent', () => {
  it('runs the skill again, carrying the plan run it approves', () => {
    const e = buildApprovalEvent({ skill: 's', pageId: PAGE, planRunId: '01RUN' });
    expect(e.label_skill).toBe('s');
    expect(e.instance_page_id).toBe(PAGE);
    expect(e.provenance).toEqual({ manual: { mode: 'run', approved_plan_run_id: '01RUN' } });
  });

  it('keeps the harness the plan was made with', () => {
    expect(
      buildApprovalEvent({ skill: 's', pageId: PAGE, planRunId: 'r', harness: 'claude' })
        .provenance,
    ).toEqual({ manual: { mode: 'run', approved_plan_run_id: 'r', harness: 'claude' } });
  });

  it('refuses an approval that names no plan', () => {
    expect(() => buildApprovalEvent({ skill: 's', pageId: PAGE, planRunId: ' ' })).toThrow(/plan/);
  });
});

describe('manualModeFor', () => {
  it('maps the split button’s modes onto the gateway’s', () => {
    expect(manualModeFor('background')).toBe('run');
    expect(manualModeFor('plan')).toBe('plan');
  });

  it('has no gateway mode for the terminal, which is not a captured event', () => {
    expect(manualModeFor('terminal')).toBeUndefined();
  });
});
