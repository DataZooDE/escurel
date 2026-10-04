import type { AdminState } from '../auth/adminState';
import type { LineageNode } from '../client/types';
import type { RunControl } from '../shared/protocol';
import { runControls } from './controls';

/**
 * What the HOST knows about one run, from wherever it loaded it (the run page's own events, or the
 * thread's lineage). Both surfaces feed this one shape, so they offer the same controls for the
 * same run and resolve them into the same commands. Every id here comes from the host's data,
 * never from a webview message.
 */
export interface RunFacts {
  runId: string;
  status: string;
  admin: AdminState;
  /** The skill of the event that TRIGGERED the run. Absent when that event is not readable. */
  skill?: string | undefined;
  /** The page the run was started on. */
  targetPageId?: string | undefined;
  /** The event the run was triggered by: what a requeue puts back. */
  triggerEventId?: string | undefined;
}

export type ResolvedControl = { command: string; arg: string | Record<string, string> };

const asMap = (nodes: Iterable<LineageNode>): Map<string, LineageNode> =>
  new Map([...nodes].map((n) => [n.id, n]));

/**
 * The event that triggered a run: the run's lineage parent, and only when that parent is an event
 * the lineage actually lists. Lineage prunes what the caller may not read, so a pruned trigger is
 * absent, and nothing else is substituted for it.
 */
function triggerEvent(nodes: Map<string, LineageNode>, runId: string): LineageNode | undefined {
  const parentId = nodes.get(runId)?.parent;
  const parent = parentId ? nodes.get(parentId) : undefined;
  return parent?.type === 'event' ? parent : undefined;
}

/** The skill of the event that triggered the run (a follow-on run is NOT its thread root's skill). */
export function triggerSkill(nodes: Iterable<LineageNode>, runId: string): string | undefined {
  const skill = triggerEvent(asMap(nodes), runId)?.label_skill;
  return typeof skill === 'string' && skill ? skill : undefined;
}

/** The run's facts from a loaded lineage. `undefined` fields are unknown, not guessed. */
export function factsFromLineage(
  runId: string,
  nodes: Iterable<LineageNode> | Map<string, LineageNode>,
  admin: AdminState,
  status?: string,
): RunFacts {
  const byId = nodes instanceof Map ? nodes : asMap(nodes);
  const run = byId.get(runId);
  const trigger = triggerEvent(byId, runId);
  const skill = trigger?.label_skill;
  const target = run?.target_page_id;
  return {
    runId,
    admin,
    status: status ?? run?.state ?? '',
    skill: typeof skill === 'string' && skill ? skill : undefined,
    targetPageId: typeof target === 'string' && target ? target : undefined,
    triggerEventId: trigger?.id,
  };
}

/** Suppress controls whose identifiers cannot be read for this run: they could not be resolved. */
export function visibleControls(
  controls: readonly RunControl[],
  facts: Pick<RunFacts, 'skill' | 'targetPageId' | 'triggerEventId'>,
): RunControl[] {
  return controls.filter((control) => {
    if (control.action === 'approve') return !!facts.skill && facts.targetPageId !== undefined;
    if (control.action === 'fix-skill') return !!facts.skill;
    if (control.action === 'requeue') return !!facts.triggerEventId;
    return true;
  });
}

/** The controls this run offers: by status and role, then only those that can be resolved. */
export function offeredControls(facts: RunFacts): RunControl[] {
  return visibleControls(runControls(facts.status, facts.admin), facts);
}

/** One control, resolved into a command. The caller has already checked it is offered AND enabled. */
export function resolveControl(
  facts: RunFacts,
  action: RunControl['action'],
): ResolvedControl | undefined {
  switch (action) {
    case 'cancel':
      return { command: 'escurel.cancelRun', arg: { runId: facts.runId } };
    case 'retry':
      return { command: 'escurel.retryRun', arg: { runId: facts.runId } };
    case 'requeue':
      return facts.triggerEventId
        ? { command: 'escurel.requeue', arg: { eventId: facts.triggerEventId } }
        : undefined;
    case 'approve':
      return facts.skill && facts.targetPageId !== undefined
        ? {
            command: 'escurel.approvePlan',
            arg: { runId: facts.runId },
          }
        : undefined;
    case 'fix-skill':
      return facts.skill ? { command: 'escurel.viewSkill', arg: facts.skill } : undefined;
  }
}
