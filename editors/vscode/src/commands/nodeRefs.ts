import type { Event } from '../client';
import { pageSkill } from '../shared/pageId';
import { formatRelativeTime } from '../views/inboxModel';

/** Where a row of any tree leads. Each field is present only when the row really knows it. */
export interface NodeRefs {
  rootEventId?: string;
  runId?: string;
  skill?: string;
  pageId?: string;
}

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);
const str = (v: unknown): string | undefined => (typeof v === 'string' && v ? v : undefined);

function put(into: NodeRefs, key: keyof NodeRefs, value: string | undefined): void {
  if (value) into[key] = value;
}

/**
 * Read the refs a tree row carries. The Inbox, Awaiting You and Knowledge each hand a command their
 * own row shape; this is the one place that knows all of them, so a command can ask any row
 * "which thread are you in, which run, which skill, which page?" without caring which tree it came from.
 */
export function nodeRefs(arg: unknown): NodeRefs {
  const refs: NodeRefs = {};
  if (!isRecord(arg)) return refs;
  switch (arg.kind) {
    case 'event': {
      const event = isRecord(arg.event) ? arg.event : {};
      put(refs, 'rootEventId', str(event.root_event_id) ?? str(event.event_id));
      put(refs, 'skill', str(event.label_skill));
      put(refs, 'pageId', str(arg.pageId) ?? str(event.instance_page_id));
      break;
    }
    case 'confirm_gate': {
      const event = isRecord(arg.event) ? arg.event : {};
      put(refs, 'rootEventId', str(event.root_event_id) ?? str(event.event_id));
      put(refs, 'skill', str(event.label_skill));
      put(refs, 'pageId', str(event.instance_page_id));
      break;
    }
    case 'plan':
      put(refs, 'rootEventId', str(arg.rootEventId));
      put(refs, 'runId', str(arg.runId));
      put(refs, 'skill', str(arg.skill));
      put(refs, 'pageId', str(arg.pageId));
      break;
    case 'changeset': {
      const c = isRecord(arg.changeset) ? arg.changeset : {};
      put(refs, 'rootEventId', str(c.root_event_id));
      put(refs, 'runId', str(c.run_id));
      const first = Array.isArray(c.target_page_ids) ? str(c.target_page_ids[0]) : undefined;
      put(refs, 'skill', first ? pageSkill(first) : undefined);
      break;
    }
    case 'draft': {
      const d = isRecord(arg.draft) ? arg.draft : {};
      put(refs, 'rootEventId', str(d.root_event_id));
      put(refs, 'runId', str(d.run_id));
      const target = str(d.target_page_id);
      put(refs, 'pageId', target);
      put(refs, 'skill', target ? pageSkill(target) : undefined);
      break;
    }
    case 'instance':
      put(refs, 'pageId', str(arg.pageId));
      put(refs, 'skill', str(arg.skill));
      break;
    case 'skill': {
      const s = arg.skill;
      put(refs, 'skill', isRecord(s) ? str(s.id) : str(s));
      break;
    }
  }
  return refs;
}

export interface SkillThreadItem {
  label: string;
  description: string;
  rootEventId: string;
}

/**
 * The threads a skill started: the signals filed under it, one row per thread, newest first. System
 * rows (run-started, review transitions) are bookkeeping, not something a person started.
 */
export function skillThreadItems(events: readonly Event[], now = Date.now()): SkillThreadItem[] {
  const seen = new Set<string>();
  const items: { at: number; item: SkillThreadItem }[] = [];
  for (const e of events) {
    if (e.kind === 'system') continue;
    const rootEventId = e.root_event_id ?? e.event_id;
    if (seen.has(rootEventId)) continue;
    seen.add(rootEventId);
    const at = e.at ? Date.parse(e.at) : 0;
    const age = formatRelativeTime(e.at, now);
    items.push({
      at,
      item: {
        label: e.title || e.label_skill,
        description: [age, e.status].filter(Boolean).join(' · '),
        rootEventId,
      },
    });
  }
  return items.sort((a, b) => b.at - a.at).map((x) => x.item);
}
