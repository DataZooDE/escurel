// PageModel from `expand` + the skill row — pure, shared with the webview tests.
import { skillActionViews } from './actions';
import type { ExpandResponse, Skill, SkillField } from '../client/types';
import type { ActionView, FieldView, PageModel } from './protocol';

const HIDDEN = new Set(['type', 'skill', 'id']);
const TITLE_KEYS = ['title', 'name', 'subject', 'label'];

export function titleCase(id: string): string {
  const s = id.replace(/[-_]+/g, ' ').trim();
  return s.charAt(0).toUpperCase() + s.slice(1);
}

export function buildPageModel(e: ExpandResponse, skill: Skill): PageModel {
  const fm = e.frontmatter ?? {};
  const slug = e.page?.slug ?? e.page?.page_id.split('/').pop()?.replace(/\.md$/, '') ?? '';
  const title =
    (TITLE_KEYS.map((k) => fm[k]).find((v) => typeof v === 'string' && v.trim()) as
      string | undefined) ?? slug;
  const declared: SkillField[] = skill.fields?.length
    ? skill.fields
    : Object.keys(fm)
        .filter((k) => !HIDDEN.has(k))
        .map((name) => ({ name, kind: 'string', required: false }));
  const fields = declared.map((f) => fieldView(f, fm[f.name]));
  const summary = typeof fm.summary === 'string' ? fm.summary : undefined;
  const autonomy =
    skill.autonomy === 'auto' || skill.autonomy === 'confirm' ? skill.autonomy : 'review';
  const actions: ActionView[] = skillActionViews(skill.actions).filter((action) => {
    if (action.skill === 'evolve_validate') {
      return fm.next_validation_action === 'evolve_validate_winner'
        && (fm.status === 'completed' || fm.status === 'finished')
        && typeof fm.best_program_id === 'number'
        && Number.isSafeInteger(fm.best_program_id)
        && !!e.content_sha256;
    }
    if (action.skill === 'evolve_publish_candidate') {
      return fm.next_candidate_action === 'evolve_publish_candidate'
        && fm.status === 'passed' && fm.effective_passed === true
        && typeof fm.winner_program_id === 'number'
        && Number.isSafeInteger(fm.winner_program_id)
        && typeof fm.report_sha256 === 'string'
        && /^[a-f0-9]{64}$/i.test(fm.report_sha256)
        && !!e.content_sha256;
    }
    return true;
  });
  return {
    pageId: e.page?.page_id ?? '',
    contentSha256: e.content_sha256,
    title,
    skill: {
      id: skill.id,
      description: skill.description,
      summary: skill.summary,
      autonomy,
      layer: skill.layer,
      readOnly: skill.layer !== 'overlay',
      backend: skill.backend.kind,
    },
    fields,
    summary,
    body: e.body,
    lastWrittenBy: e.page?.last_written_by,
    editable: false,
    actions,
  };
}

const WIKILINK = /\[\[([A-Za-z0-9_.-]+)::([A-Za-z0-9_./-]+)(?:[#@|][^\]]*)?\]\]/g;
/** What a bare `[[skill::id]]` looks like once YAML has eaten the brackets. */
const BARE = /^([A-Za-z0-9_.-]+)::([A-Za-z0-9_./-]+)$/;

/**
 * The wikilinks a frontmatter value holds, whatever shape YAML gave it: a
 * quoted `"[[skill::id]]"` string, the nested list `[["skill::id"]]` that an
 * unquoted `[[skill::id]]` parses into, a list of either, or prose with links
 * in it. Deduplicated, in order.
 */
export function wikilinksOf(value: unknown): { skill: string; id: string; wikilink: string }[] {
  const out: { skill: string; id: string; wikilink: string }[] = [];
  const seen = new Set<string>();
  const add = (skill: string, id: string) => {
    const wikilink = `[[${skill}::${id}]]`;
    if (seen.has(wikilink)) return;
    seen.add(wikilink);
    out.push({ skill, id, wikilink });
  };
  const walk = (v: unknown, nested: boolean) => {
    if (Array.isArray(v)) return void v.forEach((item) => walk(item, true));
    if (typeof v !== 'string') return;
    const matches = [...v.matchAll(WIKILINK)];
    if (matches.length) return void matches.forEach((m) => add(m[1]!, m[2]!));
    // Only inside a list: an unquoted `[[a::b]]` reaches us as [["a::b"]].
    const bare = nested ? BARE.exec(v.trim()) : null;
    if (bare) add(bare[1]!, bare[2]!);
  };
  walk(value, false);
  return out;
}

export function fieldView(f: SkillField, value: unknown): FieldView {
  const render = f.render ?? defaultRender(f.kind);
  const view: FieldView = {
    name: f.name,
    label: f.label ?? f.name,
    kind: f.kind,
    render,
    required: f.required,
    value,
    display: displayOf(f.kind, render, value),
    values: f.values,
  };
  // A value that holds wikilinks renders as instance links, whether or not
  // the skill declared the field as `kind: link` — an untyped corpus declares
  // no fields at all, and its values still point at instances.
  const links = wikilinksOf(value);
  if (links.length) {
    view.links = links;
    view.display = links.map((l) => l.id).join(', ');
  }
  return view;
}

function defaultRender(kind: string): string {
  switch (kind) {
    case 'date':
    case 'datetime':
    case 'link':
      return kind;
    case 'enum':
      return 'badge';
    default:
      return 'text';
  }
}

function displayOf(kind: string, render: string, value: unknown): string {
  if (value === undefined || value === null) return '';
  if (kind === 'bool' || typeof value === 'boolean') return value ? 'yes' : 'no';
  if (render === 'money' && typeof value === 'number')
    return new Intl.NumberFormat('en-US', {
      minimumFractionDigits: 2,
      maximumFractionDigits: 2,
    }).format(value);
  if (typeof value === 'number') return String(value);
  if (typeof value === 'string') return render === 'markdown' && kind !== 'string' ? value : value;
  return JSON.stringify(value);
}
