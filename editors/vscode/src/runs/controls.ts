import type { CaptureEventRequest } from '../client';
import type { RunControl } from '../shared/protocol';
import type { AdminState } from '../auth/adminState';

/** The label a control request is captured under (gateway: `RUN_CONTROL_LABEL`). */
export const RUN_CONTROL_LABEL = 'escurel:run-control';

export type ControlRequest =
  | { action: 'cancel' | 'retry'; runId?: string | undefined; reason?: string | undefined }
  | { action: 'requeue'; eventId?: string | undefined; reason?: string | undefined }
  | { action: 'pause' | 'resume'; reason?: string | undefined };

const need = (value: string | undefined, what: string, action: string): string => {
  const v = value?.trim();
  if (!v) throw new Error(`${action} needs ${what}`);
  return v;
};

/**
 * A control request as the gateway wants it (tools_control.rs): a JSON body on an
 * `escurel:run-control` event. Only the asks are sent. The gateway replaces the event's kind,
 * its target page and the `control` block, and writes `requested_by` from the token, so none of
 * those are sent. What the gateway would refuse as malformed is refused here first.
 */
export function buildControlEvent(req: ControlRequest): CaptureEventRequest {
  const body: Record<string, string> = { action: req.action };
  if (req.action === 'cancel' || req.action === 'retry') {
    body.run_id = need(req.runId, 'the run (a run id)', req.action);
  } else if (req.action === 'requeue') {
    body.event_id = need(req.eventId, 'the dead-lettered event (an event id)', req.action);
  }
  const reason = req.reason?.trim();
  if (reason) body.reason = reason;
  return {
    label_skill: RUN_CONTROL_LABEL,
    source: 'workbench',
    mime: 'application/json',
    body: JSON.stringify(body),
  };
}

const CONTROL = {
  cancel: { action: 'cancel', label: 'Cancel run' },
  retry: { action: 'retry', label: 'Retry' },
  requeue: { action: 'requeue', label: 'Requeue' },
  approve: { action: 'approve', label: 'Approve plan' },
  fix: { action: 'fix-skill', label: 'Fix skill' },
} as const;

/**
 * What to offer on a run in this state. Requeue is admin-only: for a caller known not to be an
 * admin it is shown DEACTIVATED with the reason (owner decision), and when admin-ness is
 * unknown it stays enabled and the gateway decides.
 */
export function runControls(status: string, admin: AdminState): RunControl[] {
  const on = (c: { action: RunControl['action']; label: string }): RunControl => ({
    ...c,
    enabled: true,
  });
  const requeue = (): RunControl =>
    admin === 'not-admin'
      ? {
          ...CONTROL.requeue,
          enabled: false,
          disabledReason: 'Only an admin can requeue a dead letter.',
        }
      : on(CONTROL.requeue);
  switch (status.trim().toLowerCase()) {
    case 'running':
      return [on(CONTROL.cancel)];
    case 'planned':
      return [on(CONTROL.approve)];
    case 'failed':
      return [on(CONTROL.retry), on(CONTROL.fix)];
    case 'dead_letter':
      return [on(CONTROL.retry), requeue(), on(CONTROL.fix)];
    case 'cancelled':
      return [on(CONTROL.retry)];
    default:
      return [];
  }
}
