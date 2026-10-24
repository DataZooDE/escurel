import {
  COMMAND_ID,
  DEFAULT_FIELD_LENGTH,
  ITEM_ID,
  MAX_ENUM_VALUES,
  MAX_FIELDS,
  MAX_FIELD_LENGTH,
  cleanCommand,
  cleanSections,
  type AugmentActionView,
  type AugmentCommand,
  type AugmentFieldKind,
  type AugmentFieldView,
  type AugmentFormView,
  type AugmentSection,
  type PageAugmentView,
} from '../shared/augment';
import { cleanOpt, cleanText } from '../shared/untrustedText';
import { withTimeout } from './contributions';
import type {
  AugmentActionData,
  AugmentFormData,
  CreateDraftArgs,
  PageAugment,
  PageAugmentContext,
} from './types';

interface Disposable {
  dispose(): void;
}

/** A provider that does not answer within this costs the page only its own contribution. */
export const AUGMENT_TIMEOUT_MS = 5_000;
const SKILL_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const FORM_ID = /^[a-z][a-z0-9._-]{0,63}$/;
const FIELD_NAME = /^[A-Za-z][A-Za-z0-9_]{0,63}$/;
const FIELD_KINDS: readonly AugmentFieldKind[] = ['text', 'textarea', 'enum', 'bool'];
const PAGE_ID = /^[A-Za-z0-9][A-Za-z0-9._/-]{0,509}$/;
const MAX_DRAFT_CONTENT = 256 * 1024;

/**
 * Who extends the page view of which skill. Unlike a renderer, an augment is additive, so several
 * companions may extend the same skill.
 */
export class PageAugmentRegistry {
  private readonly entries = new Map<string, PageAugment[]>();
  private readonly listeners = new Set<() => void>();

  get size(): number {
    let n = 0;
    for (const l of this.entries.values()) n += l.length;
    return n;
  }

  has(skill: string): boolean {
    return (this.entries.get(skill)?.length ?? 0) > 0;
  }

  for(skill: string): PageAugment[] {
    return [...(this.entries.get(skill) ?? [])];
  }

  onDidChange(listener: () => void): Disposable {
    this.listeners.add(listener);
    return { dispose: () => void this.listeners.delete(listener) };
  }

  register(skill: string, augment: PageAugment): Disposable {
    if (typeof skill !== 'string' || !SKILL_ID.test(skill)) {
      throw new Error('registerPageAugment: invalid skill id');
    }
    if (!augment || typeof augment !== 'object') {
      throw new Error('registerPageAugment: augment is missing');
    }
    const { sections, actions, forms } = augment;
    if (sections === undefined && actions === undefined && forms === undefined) {
      throw new Error('registerPageAugment: give at least one of sections, actions, forms');
    }
    if (sections !== undefined && typeof sections !== 'function') {
      throw new Error('registerPageAugment: sections must be a function');
    }
    if (actions !== undefined && typeof actions !== 'function') {
      throw new Error('registerPageAugment: actions must be a function');
    }
    if (forms !== undefined) {
      if (!Array.isArray(forms)) throw new Error('registerPageAugment: forms must be a list');
      const ids = new Set<string>();
      for (const f of forms) {
        if (!f || typeof f.id !== 'string' || !FORM_ID.test(f.id) || ids.has(f.id)) {
          throw new Error('registerPageAugment: invalid or duplicate form id');
        }
        ids.add(f.id);
        if (typeof f.toDraft !== 'function') {
          throw new Error('registerPageAugment: a form needs toDraft');
        }
      }
    }
    const list = this.entries.get(skill) ?? [];
    list.push(augment);
    this.entries.set(skill, list);
    this.changed();
    return {
      dispose: () => {
        const cur = this.entries.get(skill);
        const at = cur?.indexOf(augment) ?? -1;
        if (cur && at >= 0) {
          cur.splice(at, 1);
          if (!cur.length) this.entries.delete(skill);
          this.changed();
        }
      },
    };
  }

  private changed(): void {
    for (const l of [...this.listeners]) l();
  }
}

interface FormEntry {
  def: AugmentFormData;
  view: AugmentFormView;
}

/** What the host remembers of the last page it sent: the webview names handles, the host resolves them. */
export interface AugmentTable {
  commands: Map<string, AugmentCommand>;
  forms: Map<string, FormEntry>;
}

export interface LoadedAugment {
  view: PageAugmentView;
  table: AugmentTable;
}

function cleanField(raw: unknown): AugmentFieldView | undefined {
  const f = raw as Record<string, unknown> | undefined;
  if (!f || typeof f.name !== 'string' || !FIELD_NAME.test(f.name)) return undefined;
  if (!FIELD_KINDS.includes(f.kind as AugmentFieldKind)) return undefined;
  const label = cleanText(typeof f.label === 'string' ? f.label : f.name, 80);
  const kind = f.kind as AugmentFieldKind;
  const maxLength = Math.min(
    MAX_FIELD_LENGTH,
    Number.isFinite(f.maxLength) && (f.maxLength as number) > 0
      ? Math.trunc(f.maxLength as number)
      : DEFAULT_FIELD_LENGTH,
  );
  let values: string[] | undefined;
  if (kind === 'enum') {
    values = (Array.isArray(f.values) ? f.values : [])
      .filter((v): v is string => typeof v === 'string' && v !== '')
      .slice(0, MAX_ENUM_VALUES)
      .map((v) => cleanText(v, 120));
    if (!values.length) return undefined;
  }
  return {
    name: f.name,
    label,
    kind,
    required: f.required === true,
    maxLength,
    ...(values ? { values } : {}),
  };
}

function cleanForm(index: number, def: AugmentFormData): FormEntry | undefined {
  const fields = (Array.isArray(def.fields) ? def.fields : [])
    .slice(0, MAX_FIELDS)
    .map(cleanField)
    .filter((f): f is AugmentFieldView => !!f);
  const title = cleanText(typeof def.title === 'string' ? def.title : '', 120);
  if (!fields.length || !title) return undefined;
  const view: AugmentFormView = {
    id: `${index}:${def.id}`,
    title,
    submitLabel: cleanText(typeof def.submitLabel === 'string' ? def.submitLabel : 'Propose', 40) || 'Propose',
    fields,
  };
  return { def, view };
}

/**
 * Reads what every companion registered for `skill` and folds it into one view. What a provider
 * returns is data from another extension: it is cleaned and bounded, commands stay in the host's
 * table, and one provider that fails costs only its own contribution (the view then says so).
 */
export async function loadAugment(
  registry: PageAugmentRegistry,
  skill: string,
  base: Omit<PageAugmentContext, 'signal'>,
  timeoutMs = AUGMENT_TIMEOUT_MS,
): Promise<LoadedAugment | undefined> {
  const augments = registry.for(skill);
  if (!augments.length) return undefined;
  const table: AugmentTable = { commands: new Map(), forms: new Map() };
  const view: PageAugmentView = { sections: [], actions: [], forms: [] };
  let n = 0;
  let listKeys = 0;
  const run = async <T>(f: ((c: PageAugmentContext) => T | Promise<T>) | undefined): Promise<T | undefined> => {
    if (!f) return undefined;
    try {
      return await withTimeout(async (signal) => f({ ...base, signal }), timeoutMs);
    } catch {
      view.unavailable = true;
      return undefined;
    }
  };
  for (const [index, a] of augments.entries()) {
    const [secs, acts] = await Promise.all([run(a.sections?.bind(a)), run(a.actions?.bind(a))]);
    const seen = new Set(view.sections.map((s) => s.id));
    const cleaned: AugmentSection[] = cleanSections(secs, (_sid, cmd) => {
      const key = `l:${(listKeys += 1)}`;
      table.commands.set(key, cmd);
      return key;
    }).filter((s) => !seen.has(s.id));
    for (const s of cleaned) {
      if (view.sections.length >= 12) break;
      view.sections.push(s);
    }
    for (const raw of Array.isArray(acts) ? (acts as AugmentActionData[]) : []) {
      const cmd = cleanCommand(raw?.command, raw?.args);
      const label = cleanText(typeof raw?.label === 'string' ? raw.label : '', 60);
      if (!cmd || !label || typeof raw.id !== 'string' || !ITEM_ID.test(raw.id)) continue;
      const id = `a:${(n += 1)}`;
      table.commands.set(id, cmd);
      const hint = cleanOpt(raw.hint, 200);
      const v: AugmentActionView = { id, label, ...(hint ? { hint } : {}) };
      view.actions.push(v);
    }
    for (const def of a.forms ?? []) {
      const entry = cleanForm(index, def);
      if (!entry) continue;
      table.forms.set(entry.view.id, entry);
      view.forms.push(entry.view);
    }
  }
  return { view, table };
}

/** The command behind a button or list line the webview named; nothing for a handle the host never gave. */
export function resolveAugmentAction(table: AugmentTable, id: unknown): AugmentCommand | undefined {
  if (typeof id !== 'string') return undefined;
  const c = table.commands.get(id);
  return c && COMMAND_ID.test(c.command) ? c : undefined;
}

export type AugmentSubmit = { ok: true; args: CreateDraftArgs } | { ok: false; message: string };

const refuse = (message: string): AugmentSubmit => ({ ok: false, message });

function checkValues(
  fields: readonly AugmentFieldView[],
  raw: unknown,
): { values: Record<string, unknown> } | { message: string } {
  if (!raw || typeof raw !== 'object' || Array.isArray(raw)) return { message: 'No values.' };
  const input = raw as Record<string, unknown>;
  const values: Record<string, unknown> = {};
  for (const f of fields) {
    const v = input[f.name];
    if (f.kind === 'bool') {
      values[f.name] = v === true;
      continue;
    }
    const text = typeof v === 'string' ? v : '';
    if (f.required && !text.trim()) return { message: `${f.label} is required.` };
    if ([...text].length > f.maxLength) return { message: `${f.label} is too long.` };
    if (f.kind === 'enum' && text !== '' && !f.values?.includes(text)) {
      return { message: `${f.label}: not one of the offered values.` };
    }
    values[f.name] = f.kind === 'text' ? text.replace(/[\r\n]+/g, ' ') : text;
  }
  return { values };
}

/**
 * A form submit: the values are checked against the host's own copy of the form, the companion's
 * `toDraft` turns them into a draft, and only the `create_draft` arguments survive. The webview
 * decides nothing but the values.
 */
export async function resolveAugmentSubmit(
  table: AugmentTable,
  formId: unknown,
  rawValues: unknown,
  base: Omit<PageAugmentContext, 'signal'>,
): Promise<AugmentSubmit> {
  const entry = typeof formId === 'string' ? table.forms.get(formId) : undefined;
  if (!entry) return refuse('That form is not offered on this page.');
  const checked = checkValues(entry.view.fields, rawValues);
  if ('message' in checked) return refuse(checked.message);
  let draft: unknown;
  try {
    draft = await withTimeout(
      async (signal) => entry.def.toDraft(checked.values, { ...base, signal }),
      AUGMENT_TIMEOUT_MS,
    );
  } catch {
    return refuse('The form could not prepare the proposal.');
  }
  const d = draft as Record<string, unknown> | undefined;
  if (
    !d ||
    typeof d.target_page_id !== 'string' ||
    !PAGE_ID.test(d.target_page_id) ||
    d.target_page_id.split('/').includes('..') ||
    typeof d.content !== 'string' ||
    d.content.length > MAX_DRAFT_CONTENT
  ) {
    return refuse('The form did not produce a valid proposal.');
  }
  const args: CreateDraftArgs = { target_page_id: d.target_page_id, content: d.content };
  if (typeof d.base_sha256 === 'string' && /^[0-9a-f]{64}$/i.test(d.base_sha256)) {
    args.base_sha256 = d.base_sha256;
  }
  return { ok: true, args };
}
