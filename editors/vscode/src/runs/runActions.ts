import type { RunControl, RunView } from '../shared/protocol';

export type ActionRunView = RunView & { triggerEventId?: string };
export type ResolvedRunAction = { command: string; args: string | Record<string, string> };

/** Suppress actions whose identifiers cannot be read from this run. */
export function visibleRunControls(view: ActionRunView): RunControl[] {
  return (view.controls ?? []).filter((control) => {
    if (control.action === 'approve') return !!view.skill && view.targetPageId !== undefined;
    if (control.action === 'fix-skill') return !!view.skill;
    if (control.action === 'requeue') return !!view.triggerEventId;
    return true;
  });
}

/** The panel message is untrusted. Every argument comes from the host's loaded view. */
export function resolveRunAction(
  view: ActionRunView,
  message: unknown,
): ResolvedRunAction | undefined {
  if (!message || typeof message !== 'object') return undefined;
  const m = message as Record<string, unknown>;
  if (m.type === 'view-skill') {
    return view.skill &&
      m.skill === view.skill &&
      Object.keys(m).every((key) => ['type', 'skill'].includes(key))
      ? { command: 'escurel.viewSkill', args: view.skill }
      : undefined;
  }
  if (m.type !== 'run-control' || m.runId !== view.runId || typeof m.action !== 'string')
    return undefined;
  // Extra fields are never accepted, even if they happen to match host data.
  if (Object.keys(m).some((key) => !['type', 'action', 'runId'].includes(key))) return undefined;
  const control = visibleRunControls(view).find((c) => c.action === m.action && c.enabled);
  if (!control) return undefined;
  switch (control.action) {
    case 'cancel':
      return { command: 'escurel.cancelRun', args: { runId: view.runId } };
    case 'retry':
      return { command: 'escurel.retryRun', args: { runId: view.runId } };
    case 'requeue':
      return view.triggerEventId
        ? { command: 'escurel.requeue', args: { eventId: view.triggerEventId } }
        : undefined;
    case 'approve':
      return view.skill && view.targetPageId !== undefined
        ? {
            command: 'escurel.approvePlan',
            args: { runId: view.runId, skill: view.skill, pageId: view.targetPageId },
          }
        : undefined;
    case 'fix-skill':
      return view.skill ? { command: 'escurel.viewSkill', args: view.skill } : undefined;
  }
}
