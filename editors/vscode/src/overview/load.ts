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
import type { DelayedSkillCount, OpenSkillCount, OverviewInputs } from './model';

/** At most this many skills are counted: one read each. */
export const OPEN_SKILLS_MAX = 8;
const COUNT_PAGE = 50;
const DELAYED_PAGE = 100;
const DELAYED_MAX_PAGES = 5;
const warnedUnfilterable = new Set<string>();

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

/** Read only declarations the gateway says can be filtered. Failures omit just that tile. */
export async function countDelayed(client: EscurelClient, skills: readonly Skill[]): Promise<DelayedSkillCount[]> {
  const eligible = skills.filter((s) => {
    const o = s.overview;
    return o && typeof o.tile === 'string' && o.tile.trim().length > 0 && [...o.tile].length <= 60 &&
      typeof o.status_field === 'string' && Array.isArray(o.delayed_when) && o.delayed_when.length > 0 &&
      o.delayed_when.every((v) => ['string', 'number', 'boolean'].includes(typeof v)) &&
      (s.fields?.some((f) => f.name === o.status_field) ||
        s.backend?.filterable?.some((f) => f.field === o.status_field)) &&
      (s.backend?.instances !== 'rows' || s.backend.filterable?.some((f) => f.field === o.status_field));
  });
  for (const s of skills) {
    if (s.overview && s.backend?.instances === 'rows' &&
      !s.backend.filterable?.some((f) => f.field === s.overview!.status_field) &&
      !warnedUnfilterable.has(s.id)) {
      warnedUnfilterable.add(s.id);
      log().debug(`overview: ${s.id} status field is not filterable; omitting tile`);
    }
  }
  const reads = await Promise.all(eligible.map(async (s): Promise<DelayedSkillCount | undefined> => {
    const hook = s.overview!;
    try {
      const found = new Map<string, { pageId: string; title: string; sort?: string }>();
      let pagesRead = 0;
      let truncated = false;
      for (const value of hook.delayed_when) {
        let cursor: string | undefined;
        // The page budget is shared by every delayed value: running out of it means the count is a floor.
        if (pagesRead >= DELAYED_MAX_PAGES) {
          truncated = true;
          break;
        }
        while (pagesRead < DELAYED_MAX_PAGES) {
          const page = await client.listInstancesPage({ skill_id: s.id, frontmatter_key: hook.status_field,
            frontmatter_value: String(value), limit: DELAYED_PAGE, ...(cursor ? { cursor } : {}) });
          pagesRead++;
          for (const row of page.instances) {
            const fm = row.frontmatter ?? {};
            const title = ['title', 'name', 'subject', 'label'].map((k) => fm[k])
              .find((v) => typeof v === 'string' && v.trim()) ?? row.page_id.split('/').pop()?.replace(/\.md$/, '') ?? row.page_id;
            const sort = hook.sort_field ? fm[hook.sort_field] : undefined;
            found.set(row.page_id, { pageId: row.page_id, title: cleanText(String(title), 160),
              ...(sort == null ? {} : { sort: cleanText(String(sort), 80) }) });
          }
          if (!page.next_cursor) break;
          cursor = page.next_cursor;
          if (pagesRead >= DELAYED_MAX_PAGES) truncated = true;
        }
      }
      const instances = [...found.values()];
      if (hook.sort_field) instances.sort((a, b) => (Number(b.sort) - Number(a.sort)) ||
        (b.sort ?? '').localeCompare(a.sort ?? ''));
      return { skill: s.id, title: cleanText(hook.tile, 60), count: instances.length, instances: instances.slice(0, 3),
        ...(truncated ? { truncated: true } : {}) };
    } catch (err) {
      log().debug(`overview: ${s.id} delayed tile unavailable: ${describeError(err)}`);
      return undefined;
    }
  }));
  return reads.filter((r): r is DelayedSkillCount => !!r);
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

  const [runsPart, open, delayed] = await Promise.all([
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
    countDelayed(client, skills),
  ]);

  return { awaiting, runs: runsPart.records, runner: runsPart.runner, open, delayed };
}
