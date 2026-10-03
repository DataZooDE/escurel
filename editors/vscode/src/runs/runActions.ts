import type { RunControl, RunView } from '../shared/protocol';
import { resolveControl, visibleControls, type RunFacts } from './runFacts';

export type ActionRunView = RunView & { triggerEventId?: string };
export type ResolvedRunAction = { command: string; args: string | Record<string, string> };

/** Suppress actions whose identifiers cannot be read from this run (see `visibleControls`). */
export function visibleRunControls(view: ActionRunView): RunControl[] {
  return visibleControls(view.controls ?? [], factsOf(view));
}

function factsOf(view: ActionRunView): RunFacts {
  return {
    runId: view.runId,
    status: view.status,
    admin: 'unknown',
    skill: view.skill,
    targetPageId: view.targetPageId,
    triggerEventId: view.triggerEventId,
  };
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
  const resolved = resolveControl(factsOf(view), control.action);
  return resolved ? { command: resolved.command, args: resolved.arg } : undefined;
}

/** The trace id to copy: the one the HOST holds for this run, never a string the webview sends. */
export function traceIdToCopy(view: RunView | undefined): string | undefined {
  return view?.traceId || undefined;
}

/** A "load more" is accepted only for the exact cursor the host offered with the last page. */
export function acceptLoadMore(view: RunView, after: unknown): boolean {
  return typeof after === 'number' && Number.isInteger(after) && after === view.nextAfter;
}
