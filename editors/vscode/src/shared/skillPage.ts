// The readable page of a skill (what `escurel.viewSkill` opens) — pure, shared with the webview tests.
// A skill is a recipe for one kind of work; its Markdown file is the source, not the thing to read.
import type { Event, Instance, Skill, SkillField } from '../client/types';
import { skillActionViews } from './actions';
import { skillFacts } from './freshness';
import { titleCase } from './page';
import { pageSlug } from './pageId';
import type { ActionView } from './protocol';
import { cleanText } from './untrustedText';

export const SKILL_PAGE_INSTANCES = 10;
export const SKILL_PAGE_RUNS = 8;

export interface SkillFactView {
  label: string;
  value: string;
}

export interface SkillFieldView {
  name: string;
  label: string;
  required: boolean;
  /** Plain words: the kind, what a link points to, the allowed values. */
  detail: string;
  description?: string;
}

export interface SkillInstanceView {
  pageId: string;
  title: string;
}

export interface SkillRunView {
  rootEventId: string;
  runId?: string;
  title: string;
  at: string | null;
  /** A word, not a colour: `waiting` (nobody has handled it yet), `done`, or the status as sent. */
  state: string;
  pageId?: string;
}

export interface SkillPageModel {
  id: string;
  pageId: string;
  title: string;
  description: string;
  summary?: string;
  readOnly: boolean;
  stale: boolean;
  /** The skill's own OKF provenance in short phrases (verified, generated, stale after …). */
  provenance: string[];
  facts: SkillFactView[];
  fields: SkillFieldView[];
  actions: ActionView[];
  instances: { items: SkillInstanceView[]; more: boolean };
  runs: SkillRunView[];
}

/** Messages of the skill-page webview (its own small protocol; the page-as-UI one stays untouched). */
export type SkillPageToHost =
  | { type: 'ready' }
  | { type: 'refresh' }
  | { type: 'show-raw' }
  | { type: 'open-page'; pageId: string }
  | { type: 'open-thread'; rootEventId: string }
  | { type: 'open-run'; runId: string }
  | { type: 'start-skill'; skill: string; mode: 'run' | 'plan' };

export type SkillPageToWebview =
  | { type: 'loading' }
  | { type: 'skill'; model: SkillPageModel }
  | { type: 'error'; message: string };

function fieldDetail(f: SkillField): string {
  const parts: string[] = [];
  if (f.kind === 'link') parts.push(f.target_skill ? `link to ${cleanText(f.target_skill, 80)}` : 'link');
  else if (f.kind === 'enum' && f.values?.length) parts.push(`one of ${f.values.slice(0, 50).map((v) => cleanText(String(v), 60)).join(', ')}`);
  else parts.push(cleanText(String(f.kind), 40));
  if (f.min !== undefined || f.max !== undefined) {
    parts.push(`${f.min ?? ''}…${f.max ?? ''}`);
  }
  return parts.join(', ');
}

function fieldViews(skill: Skill): SkillFieldView[] {
  if (skill.fields?.length) {
    return skill.fields.map((f) => ({
      name: cleanText(f.name, 80),
      label: cleanText(f.label?.trim() || titleCase(f.name), 80),
      required: f.required,
      detail: fieldDetail(f),
      ...(f.description ? { description: cleanText(f.description, 600) } : {}),
    }));
  }
  const required = new Set(skill.required_frontmatter);
  return [...skill.required_frontmatter, ...skill.optional_frontmatter].map((name) => ({
    name: cleanText(name, 80),
    label: cleanText(titleCase(name), 80),
    required: required.has(name),
    detail: 'text',
  }));
}

function instanceTitle(i: Instance): string {
  const fm = i.frontmatter ?? {};
  for (const k of ['title', 'name', 'subject', 'label']) {
    const v = fm[k];
    if (typeof v === 'string' && v.trim()) return cleanText(v, 160);
  }
  return pageSlug(i.page_id);
}

function runState(status: string): string {
  if (status === 'inbox') return 'waiting';
  if (status === 'processed') return 'done';
  return status;
}

function time(e: Event): number {
  const t = e.at ? Date.parse(e.at) : NaN;
  return Number.isNaN(t) ? 0 : t;
}

export function buildSkillPageModel(
  skill: Skill,
  instances: Instance[],
  events: Event[],
  now: number = Date.now(),
): SkillPageModel {
  const provenance = skillFacts(skill, now);
  const facts: SkillFactView[] = [];
  if (skill.role) facts.push({ label: 'Role', value: cleanText(skill.role, 80) });
  if (skill.folder) facts.push({ label: 'Folder', value: cleanText(skill.folder, 200) });
  if (skill.tags?.length) facts.push({ label: 'Tags', value: cleanText(skill.tags.slice(0, 30).join(', '), 300) });
  facts.push({ label: 'Backend', value: cleanText(String(skill.backend.kind), 40) });
  facts.push({
    label: 'Autonomy',
    value: skill.autonomy === 'auto' || skill.autonomy === 'confirm' ? skill.autonomy : 'review',
  });
  facts.push({ label: 'Layer', value: cleanText(String(skill.layer), 40) });
  if (skill.resource) facts.push({ label: 'Describes', value: cleanText(skill.resource, 300) });

  const runs = [...events]
    .sort((a, b) => time(b) - time(a) || b.event_id.localeCompare(a.event_id))
    .slice(0, SKILL_PAGE_RUNS)
    .map<SkillRunView>((e) => ({
      rootEventId: e.root_event_id ?? e.event_id,
      ...(e.run_id ? { runId: e.run_id } : {}),
      title: cleanText(e.title ?? '', 160) || 'Untitled event',
      at: e.at,
      state: cleanText(runState(e.status), 40),
      ...(e.instance_page_id ? { pageId: e.instance_page_id } : {}),
    }));

  return {
    id: skill.id,
    pageId: `markdown/skills/${skill.id}.md`,
    title: cleanText(skill.title?.trim() || titleCase(skill.id), 160),
    description: cleanText(skill.description ?? '', 1000),
    ...(skill.summary ? { summary: cleanText(skill.summary, 1000) } : {}),
    readOnly: skill.layer !== 'overlay',
    stale: provenance.stale,
    provenance: provenance.facts,
    facts,
    fields: fieldViews(skill),
    actions: skillActionViews(skill.actions),
    instances: {
      items: instances
        .slice(0, SKILL_PAGE_INSTANCES)
        .map((i) => ({ pageId: i.page_id, title: instanceTitle(i) })),
      more: instances.length > SKILL_PAGE_INSTANCES,
    },
    runs,
  };
}
