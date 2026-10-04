import { describe, expect, it } from 'vitest';
import { bindCandidateSelection, bindValidationSelection, buildApprovalEvent, buildStartEvent, manualModeFor } from '../../src/start/startEvent';

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

  it('titles the event by the instance, since the Inbox already prefixes the skill', () => {
    // The Inbox row reads "<label_skill> · <title>". A title that starts with the skill again made
    // it "supplier-risk · supplier-risk · order-4500123" (seen in the real window).
    const e = buildStartEvent({ skill: 'supplier-risk', pageId: PAGE, mode: 'run' });
    expect(e.title).toBe('order-4500123');
  });
});

describe('buildApprovalEvent', () => {
  it('uses one stable event ID for repeated Evolve approval of the same plan', () => {
    const first = buildApprovalEvent({ skill: 'evolve_run', pageId: PAGE, planRunId: '01RUN' });
    const retry = buildApprovalEvent({ skill: 'evolve_run', pageId: PAGE, planRunId: '01RUN' });
    expect(first.event_id).toBe('evolve-approval-01RUN');
    expect(retry.event_id).toBe(first.event_id);
  });
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

describe('bindValidationSelection', () => {
  it('uses a stable event ID and freezes the reviewed winner and page revision', () => {
    const start = buildStartEvent({ skill: 'evolve_validate', pageId: PAGE, mode: 'run' });
    const first = bindValidationSelection(start, 'a'.repeat(64), 7);
    const retry = bindValidationSelection(start, 'a'.repeat(64), 7);
    expect(first.event_id).toBe(retry.event_id);
    expect(first.provenance).toEqual({ manual: {
      mode: 'run', expected_page_sha256: 'a'.repeat(64), expected_winner_program_id: 7,
    } });
    expect(JSON.stringify(first)).not.toContain('requested_by');
    expect(() => bindValidationSelection(start, 'bad', 7)).toThrow(/reviewed/);
    expect(() => bindValidationSelection(start, 'a'.repeat(64), -1)).toThrow(/winner/);
  });
});

describe('bindCandidateSelection', () => {
  it('freezes an explicit review of the exact private report', () => {
    const start = buildStartEvent({ skill: 'evolve_publish_candidate', pageId: PAGE, mode: 'run' });
    const first = bindCandidateSelection(start, 'a'.repeat(64), 7, 'b'.repeat(64), 'reviewed both tails');
    const retry = bindCandidateSelection(start, 'a'.repeat(64), 7, 'b'.repeat(64), 'reviewed both tails');
    expect(first.event_id).not.toBe(retry.event_id);
    expect(first.provenance).toEqual({ manual: {
      mode: 'run', expected_page_sha256: 'a'.repeat(64), expected_winner_program_id: 7,
      expected_validation_report_sha256: 'b'.repeat(64), confirm: true,
      review_note: 'reviewed both tails',
    } });
    expect(() => bindCandidateSelection(start, 'bad', 7, 'b'.repeat(64), '')).toThrow(/reviewed/);
    expect(() => bindCandidateSelection(start, 'a'.repeat(64), 7, 'bad', '')).toThrow(/report/);
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

describe('a start with no target instance', () => {
  // "No target instance" is a real choice in the picker: a skill that is not about one page. An
  // empty string for instance_page_id is not "none"; the key is left out.
  it('leaves instance_page_id out, and names the event for the skill alone', () => {
    const e = buildStartEvent({ skill: 'supplier-risk', pageId: '', mode: 'run' });
    expect('instance_page_id' in e).toBe(false);
    expect(e.title).toBe('Started from the workbench');
  });

  it('does the same for an approval', () => {
    const e = buildApprovalEvent({ skill: 's', pageId: '', planRunId: '01RUN' });
    expect('instance_page_id' in e).toBe(false);
    expect(e.provenance).toEqual({ manual: { mode: 'run', approved_plan_run_id: '01RUN' } });
  });
});
