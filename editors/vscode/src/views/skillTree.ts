import type { Skill } from '../client';
import { skillRow, type SkillRow } from './knowledgeModel';

/** What kind of thing a skill is, as the Knowledge tree sorts and icons it. */
export type Role = 'record' | 'process' | 'report' | 'helper';

/** Tree order: business data first, plumbing last. */
export const ROLE_ORDER: readonly Role[] = ['record', 'process', 'report', 'helper'];

/** One themed codicon per role. */
export const ROLE_ICONS: Record<Role, string> = {
  record: 'database',
  process: 'play-circle',
  report: 'graph',
  helper: 'tools',
};

const isRole = (v: unknown): v is Role => typeof v === 'string' && ROLE_ORDER.includes(v as Role);

/**
 * The skill's role: what it DECLARES (`role:`), else a guess from what the wire carries. The wire has
 * no `render`, so a Peacock report is only recognised by declaring `role: report` or by taking run
 * `params` and having no instance `fields`. Anything unrecognised is a record, the commonest kind.
 */
export function effectiveRole(skill: Skill): { role: Role; inferred: boolean } {
  if (isRole(skill.role)) return { role: skill.role, inferred: false };
  let role: Role = 'record';
  if (skill.backend.kind === 'sql_view' || skill.id === 'query') role = 'helper';
  else if (skill.harness) role = 'process';
  else if ((skill.params?.length ?? 0) > 0 && (skill.fields?.length ?? 0) === 0) role = 'report';
  return { role, inferred: true };
}

/** What a screen reader hears for a skill row. */
export function skillAccessibleName(skill: Skill): string {
  const { role, inferred } = effectiveRole(skill);
  const autonomy =
    skill.autonomy && ['auto', 'review', 'confirm'].includes(skill.autonomy)
      ? skill.autonomy
      : 'review';
  return `${role} skill${inferred ? ' (inferred)' : ''} ${skill.id}, autonomy ${autonomy}`;
}

export interface FolderRow {
  kind: 'folder';
  /** The whole path, `sales/risk`. */
  path: string;
  /** The last segment, `risk`. */
  label: string;
  children: TreeNode[];
  /** Holds nothing but helpers: starts collapsed. */
  collapsed: boolean;
}

export type TreeNode = FolderRow | SkillRow;

const SLUG = /^[a-z0-9_-]+$/;

/** The folder's segments, or `undefined` when there is none or it is malformed (the skill goes to the top). */
function segments(folder: string | undefined): string[] | undefined {
  const path = folder?.trim();
  if (!path) return undefined;
  const parts = path.split('/');
  return parts.every((p) => SLUG.test(p)) ? parts : undefined;
}

interface Trie {
  folders: Map<string, Trie>;
  skills: Skill[];
}

const emptyTrie = (): Trie => ({ folders: new Map(), skills: [] });

function allHelpers(t: Trie): boolean {
  const own = t.skills.every((s) => effectiveRole(s).role === 'helper');
  return own && [...t.folders.values()].every(allHelpers);
}

function toNodes(t: Trie, prefix: string): TreeNode[] {
  const folders: FolderRow[] = [...t.folders.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([label, child]) => {
      const path = prefix ? `${prefix}/${label}` : label;
      return {
        kind: 'folder',
        path,
        label,
        children: toNodes(child, path),
        collapsed: allHelpers(child),
      };
    });
  const rank = (s: Skill) => ROLE_ORDER.indexOf(effectiveRole(s).role);
  const skills = [...t.skills]
    .sort((a, b) => rank(a) - rank(b) || a.id.localeCompare(b.id))
    .map(skillRow);
  return [...folders, ...skills];
}

/**
 * The Knowledge tree's skill level: folders (from each skill's `folder:`), folders first and by name,
 * then skills by role (record, process, report, helper) and name. A skill with no folder, or a
 * malformed one, sits at the top level rather than disappearing.
 */
export function buildSkillTree(skills: Skill[]): TreeNode[] {
  const root = emptyTrie();
  for (const skill of skills) {
    let node = root;
    for (const part of segments(skill.folder) ?? []) {
      let next = node.folders.get(part);
      if (!next) {
        next = emptyTrie();
        node.folders.set(part, next);
      }
      node = next;
    }
    node.skills.push(skill);
  }
  return toNodes(root, '');
}

/** What the Knowledge tree is narrowed to: a tag, free text, or both (both have to match). */
export interface SkillFilter {
  tag?: string | undefined;
  text?: string | undefined;
}

const norm = (s: string | undefined): string => (s ?? '').trim().toLowerCase();

const tagMatches = (skill: Skill, tag: string): boolean =>
  (skill.tags ?? []).some((t) => {
    const x = t.toLowerCase();
    // A tag path (`risk/analysis`) is also found by its parent (`risk`).
    return x === tag || x.startsWith(`${tag}/`);
  });

const textMatches = (skill: Skill, text: string): boolean =>
  [
    skill.id,
    skill.title,
    skill.summary,
    skill.description,
    skill.folder,
    ...(skill.tags ?? []),
  ].some((v) => v?.toLowerCase().includes(text));

/** The skills the filter lets through; the SAME array when the filter is empty. */
export function filterSkills(skills: Skill[], filter: SkillFilter): Skill[] {
  const tag = norm(filter.tag);
  const text = norm(filter.text);
  if (!tag && !text) return skills;
  return skills.filter((s) => (!tag || tagMatches(s, tag)) && (!text || textMatches(s, text)));
}

/** Every tag in use with how many skills carry it: most used first, then by name. */
export function knownTags(skills: Skill[]): { tag: string; count: number }[] {
  const counts = new Map<string, number>();
  for (const s of skills)
    for (const t of new Set((s.tags ?? []).map((x) => x.trim()).filter(Boolean)))
      counts.set(t, (counts.get(t) ?? 0) + 1);
  return [...counts.entries()]
    .map(([tag, count]) => ({ tag, count }))
    .sort((a, b) => b.count - a.count || a.tag.localeCompare(b.tag));
}

/** The view's message while a filter is on, or `undefined` when none is. */
export function describeFilter(filter: SkillFilter, shown: number): string | undefined {
  const tag = filter.tag?.trim();
  const text = filter.text?.trim();
  if (!tag && !text) return undefined;
  const parts = [tag ? `tag: ${tag}` : '', text ? `text: “${text}”` : ''].filter(Boolean);
  const count = shown === 0 ? 'no skills match' : `${shown} ${shown === 1 ? 'skill' : 'skills'}`;
  return `Filtered by ${parts.join(' and ')} — ${count}`;
}
