// PageModel from `expand` + the skill row — pure, shared with the webview tests.
import type { ExpandResponse, Skill, SkillField } from '../client/types';
import type { ActionView, FieldView, PageModel } from './protocol';

const HIDDEN = new Set(['type', 'skill', 'id']);
const TITLE_KEYS = ['title', 'name', 'subject', 'label'];

export function titleCase(id: string): string {
  const s = id.replace(/[-_]+/g, ' ').trim();
  return s.charAt(0).toUpperCase() + s.slice(1);
}

/** BACKEND_GAPS PR-2 degradation: the label derived from the two ids. */
export function actionLabel(skillId: string, instanceTitle: string): string {
  return `${titleCase(skillId)} for ${instanceTitle} with an agent`;
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
  const actions: ActionView[] = (skill.actions ?? []).map((s) => ({
    skill: s,
    label: actionLabel(s, slug || title),
  }));
  return {
    pageId: e.page?.page_id ?? '',
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

const WIKILINK = /^\[\[([A-Za-z0-9_.-]+)::([A-Za-z0-9_./-]+)(?:[#@|][^\]]*)?\]\]$/;

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
  if (f.kind === 'link' && typeof value === 'string') {
    const m = WIKILINK.exec(value.trim());
    if (m) view.link = { skill: m[1]!, id: m[2]!, wikilink: value.trim() };
    view.display = m ? m[2]! : value;
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
