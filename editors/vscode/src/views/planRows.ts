import type { Event } from '../client/types';
import { pageSlug } from '../shared/pageId';

/** A plan run that is waiting for a person to approve it. */
export interface PlanRow {
  kind: 'plan';
  id: string;
  label: string;
  description: string;
  timestamp: string;
  runId: string;
  rootEventId: string | undefined;
  skill: string | undefined;
  pageId: string | undefined;
}

const record = (v: unknown): Record<string, unknown> | undefined =>
  typeof v === 'object' && v !== null && !Array.isArray(v)
    ? (v as Record<string, unknown>)
    : undefined;

function isPlanned(event: Event): boolean {
  if (!event.run_id || !event.body) return false;
  try {
    return (JSON.parse(event.body) as { status?: unknown }).status === 'planned';
  } catch {
    return false;
  }
}

/** The run a user event approves, if it approves one (`provenance.manual.approved_plan_run_id`). */
function approvedRun(event: Event): string | undefined {
  const manual = record(record(event.provenance)?.manual);
  const id = manual?.approved_plan_run_id;
  return typeof id === 'string' && id ? id : undefined;
}

/**
 * The plans that wait for approval: runs that ended `planned` and that no event has approved yet.
 *
 * A plan run stops after writing its plan; the person approves it by starting the skill again, naming
 * the plan run. The only trace of that wait used to be a toast, so a dismissed toast lost the plan. This
 * is the persistent form, for the Awaiting queue. `runEvents` are the `escurel:run` lifecycle events,
 * `userEvents` the user-kind events (the triggers, and the approvals). Newest first, one row per run.
 */
export function planRows(runEvents: Event[], userEvents: Event[]): PlanRow[] {
  const approved = new Set<string>();
  const byId = new Map<string, Event>();
  for (const e of userEvents) {
    const run = approvedRun(e);
    if (run) approved.add(run);
    byId.set(e.event_id, e);
  }
  const startedByRun = new Map<string, Event>();
  for (const e of runEvents) {
    if (e.run_id && (e.title === 'run-started' || e.event_id.endsWith(':started'))) {
      startedByRun.set(e.run_id, e);
    }
  }

  const rows = new Map<string, PlanRow>();
  for (const e of runEvents) {
    if (!isPlanned(e) || !e.run_id || approved.has(e.run_id) || rows.has(e.run_id)) continue;
    const started = startedByRun.get(e.run_id);
    const triggerId = record(record(started?.provenance)?.runner)?.event_id;
    const trigger = typeof triggerId === 'string' ? byId.get(triggerId) : undefined;
    const skill = trigger?.label_skill;
    const pageId =
      e.instance_page_id ?? started?.instance_page_id ?? trigger?.instance_page_id ?? undefined;
    const slug = pageId ? pageSlug(pageId) : '';
    const what = [skill, slug].filter(Boolean);
    const label =
      skill && slug
        ? `Plan ready · ${skill} on ${slug}`
        : `Plan ready${what.length ? ` · ${what.join(' ')}` : ''}`;
    rows.set(e.run_id, {
      kind: 'plan',
      id: `plan:${e.run_id}`,
      label,
      description: 'Approve plan',
      timestamp: e.at ?? '',
      runId: e.run_id,
      rootEventId: e.root_event_id ?? undefined,
      skill,
      pageId,
    });
  }
  return [...rows.values()].sort((a, b) => {
    const ta = a.timestamp ? new Date(a.timestamp).getTime() : 0;
    const tb = b.timestamp ? new Date(b.timestamp).getTime() : 0;
    return tb - ta || b.id.localeCompare(a.id);
  });
}

/**
 * The root events of the runs that ended `planned`, newest first, one per run, at most `max`. The Awaiting
 * view reads each root's lineage (the gateway's `list_events` needs a selector, it cannot list "every user
 * event") to learn which skill the plan was for and whether it was approved.
 */
export function plannedRunRoots(runEvents: Event[], max = 20): string[] {
  const seen = new Set<string>();
  const roots: { root: string; at: number }[] = [];
  for (const e of runEvents) {
    if (!isPlanned(e) || !e.run_id || !e.root_event_id || seen.has(e.run_id)) continue;
    seen.add(e.run_id);
    roots.push({ root: e.root_event_id, at: e.at ? new Date(e.at).getTime() : 0 });
  }
  return roots
    .sort((a, b) => b.at - a.at)
    .slice(0, max)
    .map((r) => r.root);
}
