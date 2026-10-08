// Reads what the overview board shows. No `vscode` import: the controller owns the UI. Everything
// here is data the other views already read, so the board adds no new gateway surface.
import type { EscurelClient, Skill } from '../client';
import { log } from '../log';
import { describeError } from '../errors';
import { titleCase } from '../shared/page';
import { cleanText } from '../shared/untrustedText';
import { buildAwaitingRows } from '../views/awaitingModel';
import { loadPlanInputs } from '../views/planInputs';
import {
  emptySnapshot,
  readRunnerStatus,
  recordsFrom,
  refreshRunEvents,
  resolveSkills,
} from '../views/runsLoader';
import { describeRunner } from '../views/runsModel';
import type { OpenSkillCount, OverviewInputs } from './model';

/** At most this many skills are counted: one read each. */
export const OPEN_SKILLS_MAX = 8;
const COUNT_PAGE = 50;

/** The skills worth counting records of: records and processes, never events, reports, helpers or the engine's own. */
export function pickOpenSkills(skills: readonly Skill[]): { id: string; title: string }[] {
  return skills
    .filter(
      (s) =>
        !s.is_event_typed &&
        !s.id.startsWith('escurel:') &&
        s.role !== 'report' &&
        s.role !== 'helper',
    )
    .slice(0, OPEN_SKILLS_MAX)
    .map((s) => ({ id: s.id, title: cleanText(s.title || titleCase(s.id), 80) }));
}

async function countOpen(
  client: EscurelClient,
  skills: readonly Skill[],
): Promise<OpenSkillCount[]> {
  const picked = pickOpenSkills(skills);
  const counted = await Promise.all(
    picked.map(async (s) => {
      try {
        const page = await client.listInstancesPage({ skill_id: s.id, limit: COUNT_PAGE });
        return {
          skill: s.id,
          title: s.title,
          count: page.instances.length,
          more: page.next_cursor !== null,
        };
      } catch {
        return undefined; // one unreadable skill must not blank the tile
      }
    }),
  );
  return counted
    .filter((c): c is OpenSkillCount => !!c && c.count > 0)
    .sort((a, b) => b.count - a.count);
}

/** What the board is built from. The queue must load (it is the board's reason to exist); the rest degrades. */
export async function loadOverviewInputs(
  client: EscurelClient,
  skillCache: Map<string, string>,
): Promise<Omit<OverviewInputs, 'focusOn' | 'nowMs'>> {
  const [changesets, drafts, inboxPage, skills, plans] = await Promise.all([
    client.listChangesets(),
    client.listDrafts(),
    client.listInbox(),
    client.listSkills(),
    loadPlanInputs(client),
  ]);
  const awaiting = buildAwaitingRows({
    changesets,
    drafts,
    events: inboxPage.events,
    skills,
    runEvents: plans.runEvents,
    userEvents: plans.userEvents,
  });

  const [runsPart, open] = await Promise.all([
    (async () => {
      try {
        const [status, snapshot] = await Promise.all([
          readRunnerStatus(client),
          refreshRunEvents(client, emptySnapshot(), { maxPages: 2, wantEnded: 15 }),
        ]);
        const live = status.event?.body
          ? (JSON.parse(status.event.body) as { live_runs?: { run_id: string }[] }).live_runs
          : undefined;
        const liveIds = live ? new Set(live.map((r) => r.run_id)) : undefined;
        let records = recordsFrom(snapshot, Date.now(), liveIds, skillCache);
        const missing = records
          .filter((r) => !r.skill && r.triggerEventId)
          .map((r) => r.triggerEventId!);
        if (await resolveSkills(client, missing, skillCache)) {
          records = recordsFrom(snapshot, Date.now(), liveIds, skillCache);
        }
        const runner = describeRunner(
          status.event ? { at: status.event.at, body: status.event.body } : null,
          Date.now(),
          { isAdmin: false, intervalMs: status.intervalMs },
        );
        return { records, runner };
      } catch (err) {
        log().debug(`overview: runs unavailable: ${describeError(err)}`);
        return { records: [], runner: undefined };
      }
    })(),
    countOpen(client, skills).catch(() => []),
  ]);

  return { awaiting, runs: runsPart.records, runner: runsPart.runner, open };
}
