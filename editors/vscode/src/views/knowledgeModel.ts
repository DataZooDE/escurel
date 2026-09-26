import type { Instance, Skill } from '../client';
import { pageSlug } from '../shared/pageId';

/** The chips the mock shows beside a skill: autonomy, event-typed, a non-markdown backend, a non-overlay layer, shadows. */
export function chipsForSkill(s: Skill): string[] {
  const chips = [
    s.autonomy && ['auto', 'review', 'confirm'].includes(s.autonomy) ? s.autonomy : 'review',
  ];
  if (s.is_event_typed) chips.push('event-typed');
  if (s.backend.kind !== 'markdown') chips.push(s.backend.kind);
  if (s.layer !== 'overlay') chips.push(s.layer);
  if (s.shadows) chips.push('shadows');
  return chips;
}

/** A pack-imported base page is read-only at this node (REQ-LAYER-04); only `overlay` is editable. */
export function isReadOnlySkill(s: Skill): boolean {
  return s.layer !== 'overlay';
}

export interface SkillRow {
  kind: 'skill';
  skill: Skill;
  label: string;
  description: string;
  readOnly: boolean;
}

export function skillRow(skill: Skill): SkillRow {
  return {
    kind: 'skill',
    skill,
    label: skill.id,
    description: chipsForSkill(skill).join(' · '),
    readOnly: isReadOnlySkill(skill),
  };
}

export interface InstanceRow {
  kind: 'instance';
  pageId: string;
  skill: string;
  label: string;
  description: string;
}

const TITLE_KEYS = ['title', 'name', 'summary', 'subject', 'label'];

export function instanceRow(i: Instance): InstanceRow {
  const slug = pageSlug(i.page_id, i.skill);
  const fm = i.frontmatter ?? {};
  const title = TITLE_KEYS.map((k) => fm[k]).find(
    (v) => typeof v === 'string' && v.trim() && v !== slug,
  ) as string | undefined;
  return {
    kind: 'instance',
    pageId: i.page_id,
    skill: i.skill,
    label: slug,
    description: title ?? '',
  };
}
