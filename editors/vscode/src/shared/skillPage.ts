// The readable page of a skill (what `escurel.viewSkill` opens) — pure, shared with the webview tests.
// A skill is a recipe for one kind of work; its Markdown file is the source, not the thing to read.
import type { Event, Instance, Skill, SkillField } from '../client/types';
import { skillActionViews } from './actions';
import { backendLabel } from './backendLabel';
import { skillFacts } from './freshness';
import { titleCase } from './page';
import { pageSlug } from './pageId';
import type { ActionView } from './protocol';

export const SKILL_PAGE_INSTANCES = 10;
export const SKILL_PAGE_RUNS = 8;

export interface SkillFactView {
  label: string;
  value: string;
  /** One sentence for a tooltip: what the label means. */
  hint?: string;
}

/** What the skill's gate means for an agent's changes, in plain words. */
function agentChanges(autonomy: string | undefined): string {
  if (autonomy === 'auto') return 'applied without review';
  if (autonomy === 'confirm') return 'agent asks you first';
  return 'wait for your approval';
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
  if (f.kind === 'link') parts.push(f.target_skill ? `link to ${f.target_skill}` : 'link');
  else if (f.kind === 'enum' && f.values?.length) parts.push(`one of ${f.values.join(', ')}`);
  else parts.push(f.kind);
  if (f.min !== undefined || f.max !== undefined) {
    parts.push(`${f.min ?? ''}…${f.max ?? ''}`);
  }
  return parts.join(', ');
}

function fieldViews(skill: Skill): SkillFieldView[] {
  if (skill.fields?.length) {
    return skill.fields.map((f) => ({
      name: f.name,
      label: f.label?.trim() || titleCase(f.name),
      required: f.required,
      detail: fieldDetail(f),
      ...(f.description ? { description: f.description } : {}),
    }));
  }
  const required = new Set(skill.required_frontmatter);
  return [...skill.required_frontmatter, ...skill.optional_frontmatter].map((name) => ({
    name,
    label: titleCase(name),
    required: required.has(name),
    detail: 'text',
  }));
}

function instanceTitle(i: Instance): string {
  const fm = i.frontmatter ?? {};
  for (const k of ['title', 'name', 'subject', 'label']) {
    const v = fm[k];
    if (typeof v === 'string' && v.trim()) return v.trim();
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
  if (skill.role) facts.push({ label: 'Role', value: skill.role });
  if (skill.folder) facts.push({ label: 'Folder', value: skill.folder });
  if (skill.tags?.length) facts.push({ label: 'Tags', value: skill.tags.join(', ') });
  facts.push({
    label: 'Data from',
    value: backendLabel(skill.backend.kind),
    hint: 'Where the records of this skill are stored or read from.',
  });
  facts.push({
    label: 'Agent changes',
    value: agentChanges(skill.autonomy),
    hint: 'What happens to a change an agent proposes to a record of this skill (its autonomy setting).',
  });
  // Only worth a line when the skill is shared: an ordinary one is simply yours to edit.
  if (skill.layer !== 'overlay')
    facts.push({
      label: 'Shared from',
      value: skill.layer,
      hint: 'This skill comes from a shared skill pack and is read-only here.',
    });
  if (skill.resource) facts.push({ label: 'Describes', value: skill.resource });

  const runs = [...events]
    .sort((a, b) => time(b) - time(a) || b.event_id.localeCompare(a.event_id))
    .slice(0, SKILL_PAGE_RUNS)
    .map<SkillRunView>((e) => ({
      rootEventId: e.root_event_id ?? e.event_id,
      ...(e.run_id ? { runId: e.run_id } : {}),
      title: e.title?.trim() || 'Untitled event',
      at: e.at,
      state: runState(e.status),
      ...(e.instance_page_id ? { pageId: e.instance_page_id } : {}),
    }));

  return {
    id: skill.id,
    pageId: `markdown/skills/${skill.id}.md`,
    title: skill.title?.trim() || titleCase(skill.id),
    description: skill.description,
    ...(skill.summary ? { summary: skill.summary } : {}),
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
