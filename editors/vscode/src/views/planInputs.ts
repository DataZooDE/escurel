import type { EscurelClient } from '../client';
import type { Event } from '../client/types';
import { describeError } from '../errors';
import { log } from '../log';
import { plannedRunRoots } from './planRows';

export interface PlanInputs {
  runEvents: Event[];
  userEvents: Event[];
}

const uniqueById = (events: Event[]): Event[] => [
  ...new Map(events.map((e) => [e.event_id, e])).values(),
];

/**
 * What the 'Plan ready' rows are built from. The gateway's `list_events` needs a selector (a page, a root,
 * a run or a label), it cannot list "every user event", so this asks in steps: the run lifecycle (label
 * `escurel:run`), then for each plan that ended `planned` its root's lineage (the trigger, hence the skill),
 * then the events under each such skill (an approval is a user event of the same skill, naming the plan
 * run). A failure costs the plan rows, never the rest of the Awaiting queue.
 */
export async function loadPlanInputs(
  client: Pick<EscurelClient, 'listEvents'>,
): Promise<PlanInputs> {
  let runEvents: Event[] = [];
  let userEvents: Event[] = [];
  try {
    runEvents = (
      await client.listEvents({
        label_skill: 'escurel:run',
        include_system: true,
        newest_first: true,
        limit: 100,
      })
    ).events;
    const roots = plannedRunRoots(runEvents);
    if (roots.length === 0) return { runEvents, userEvents };

    const lineages = await Promise.all(
      roots.map((root) => client.listEvents({ root_event_id: root, include_system: true })),
    );
    const lineage = lineages.flatMap((p) => p.events);
    runEvents = uniqueById([...runEvents, ...lineage.filter((e) => e.kind === 'system')]);
    const triggers = lineage.filter((e) => e.kind !== 'system');
    const skills = [...new Set(triggers.map((e) => e.label_skill))];
    const bySkill = await Promise.all(
      skills.map((skill) =>
        client.listEvents({ label_skill: skill, newest_first: true, limit: 100 }),
      ),
    );
    userEvents = uniqueById([...triggers, ...bySkill.flatMap((p) => p.events)]);
  } catch (e) {
    log().warn(`escurel: plan rows: ${describeError(e)}`);
  }
  return { runEvents, userEvents };
}
