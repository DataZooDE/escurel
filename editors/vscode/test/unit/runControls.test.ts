import { describe, expect, it } from 'vitest';
import { buildControlEvent, isAdminAction, runControls } from '../../src/runs/controls';

// The contract is the gateway's (tools_control.rs): cancel/retry name a run, requeue names the
// dead-lettered event, pause/resume are tenant-wide, and the last three are admin-only.
describe('buildControlEvent', () => {
  it('captures a system-bound run-control event with a JSON body', () => {
    const e = buildControlEvent({ action: 'cancel', runId: '01RUN', reason: 'wrong target' });
    expect(e.label_skill).toBe('escurel:run-control');
    expect(e.source).toBe('workbench');
    expect(JSON.parse(e.body!)).toEqual({
      action: 'cancel',
      run_id: '01RUN',
      reason: 'wrong target',
    });
  });

  it('omits an empty reason', () => {
    expect(
      JSON.parse(buildControlEvent({ action: 'retry', runId: 'r', reason: ' ' }).body!),
    ).toEqual({ action: 'retry', run_id: 'r' });
  });

  it('names the event to put back on a requeue', () => {
    expect(JSON.parse(buildControlEvent({ action: 'requeue', eventId: '01EV' }).body!)).toEqual({
      action: 'requeue',
      event_id: '01EV',
    });
  });

  it('pause and resume name nothing: they are tenant-wide', () => {
    expect(JSON.parse(buildControlEvent({ action: 'pause' }).body!)).toEqual({ action: 'pause' });
    expect(JSON.parse(buildControlEvent({ action: 'resume' }).body!)).toEqual({ action: 'resume' });
  });

  it('refuses what the gateway would refuse, before sending it', () => {
    expect(() => buildControlEvent({ action: 'cancel', runId: '' })).toThrow(/run/);
    expect(() => buildControlEvent({ action: 'retry', runId: undefined })).toThrow(/run/);
    expect(() => buildControlEvent({ action: 'requeue', eventId: ' ' })).toThrow(/event/);
  });

  it('does not claim who asked, and sends no kind, target or control block', () => {
    const text = JSON.stringify(buildControlEvent({ action: 'cancel', runId: 'r' }));
    for (const forbidden of ['requested_by', 'instance_page_id', '"kind"', 'provenance']) {
      expect(text).not.toContain(forbidden);
    }
  });
});

describe('isAdminAction', () => {
  it('is true for the tenant-wide actions and false for the per-run ones', () => {
    expect(['pause', 'resume', 'requeue'].every((a) => isAdminAction(a as 'pause'))).toBe(true);
    expect(['cancel', 'retry'].some((a) => isAdminAction(a as 'cancel'))).toBe(false);
  });
});

describe('runControls', () => {
  const ids = (status: string, admin: 'admin' | 'not-admin' | 'unknown') =>
    runControls(status, admin).map((c) => [c.action, c.enabled]);

  it('offers Cancel on a run that is still going', () => {
    expect(ids('running', 'not-admin')).toEqual([['cancel', true]]);
  });

  it('offers Approve plan on a planned run', () => {
    expect(ids('planned', 'not-admin')).toEqual([['approve', true]]);
  });

  it('offers Retry and Fix skill on a failed run', () => {
    expect(ids('failed', 'not-admin')).toEqual([
      ['retry', true],
      ['fix-skill', true],
    ]);
  });

  it('shows Requeue on a dead letter, deactivated with a reason unless the caller is an admin', () => {
    const forHuman = runControls('dead_letter', 'not-admin');
    expect(forHuman.map((c) => c.action)).toEqual(['retry', 'requeue', 'fix-skill']);
    const requeue = forHuman.find((c) => c.action === 'requeue')!;
    expect(requeue.enabled).toBe(false);
    expect(requeue.disabledReason).toMatch(/admin/i);
    expect(ids('dead_letter', 'admin').find(([a]) => a === 'requeue')).toEqual(['requeue', true]);
  });

  it('leaves a control enabled when admin-ness is unknown, so the gateway decides', () => {
    expect(ids('dead_letter', 'unknown').find(([a]) => a === 'requeue')).toEqual(['requeue', true]);
  });

  it('offers a cancelled run a Retry and nothing else', () => {
    expect(ids('cancelled', 'admin')).toEqual([['retry', true]]);
  });

  it('offers a finished run nothing to do to it', () => {
    expect(ids('processed', 'admin')).toEqual([]);
  });

  it('reads a status case-insensitively and gives an unknown one nothing', () => {
    expect(ids('RUNNING', 'admin')).toEqual([['cancel', true]]);
    expect(ids('mystery', 'admin')).toEqual([]);
  });
});
