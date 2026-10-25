// The declarative schema a companion extension uses to add to a core view (page view, review panel).
// Pure data: the host cleans what a provider returns, the core webview draws it with text nodes only.
// Shared by both tsconfigs, so nothing here may import `vscode` or Node.
import { cleanOpt, cleanText } from './untrustedText';

export type AugmentTone = 'attention' | 'ok' | 'neutral';

export interface AugmentFact {
  label: string;
  value: string;
  tone?: AugmentTone;
}

/** A list line as the webview gets it: `key` is a handle the host can resolve, never a command. */
export interface AugmentListItem {
  label: string;
  detail?: string;
  tone?: AugmentTone;
  key?: string;
}

export type AugmentBlock =
  | { type: 'text'; text: string }
  | { type: 'facts'; rows: AugmentFact[] }
  | { type: 'table'; columns: string[]; rows: string[][] }
  | { type: 'list'; items: AugmentListItem[] };

export interface AugmentSection {
  id: string;
  title: string;
  blocks: AugmentBlock[];
  /** Said when the section has no block. */
  empty?: string;
}

export interface AugmentActionView {
  /** A handle the host handed out; the webview posts it back. */
  id: string;
  label: string;
  hint?: string;
}

export type AugmentFieldKind = 'text' | 'textarea' | 'enum' | 'bool';

export interface AugmentFieldView {
  name: string;
  label: string;
  kind: AugmentFieldKind;
  required: boolean;
  values?: string[];
  maxLength: number;
}

export interface AugmentFormView {
  id: string;
  title: string;
  submitLabel: string;
  fields: AugmentFieldView[];
}

/** What a companion adds to a page view, as the webview gets it. */
export interface PageAugmentView {
  sections: AugmentSection[];
  actions: AugmentActionView[];
  forms: AugmentFormView[];
  /** A provider failed or timed out: the view says so instead of showing less in silence. */
  unavailable?: true;
}

export const MAX_SECTIONS = 12;
export const MAX_BLOCKS = 8;
export const MAX_ROWS = 50;
export const MAX_COLUMNS = 8;
export const MAX_ITEMS = 20;
export const MAX_FIELDS = 12;
export const MAX_ENUM_VALUES = 50;
export const DEFAULT_FIELD_LENGTH = 2000;
export const MAX_FIELD_LENGTH = 20_000;
export const COMMAND_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
export const ITEM_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

const TONES: readonly AugmentTone[] = ['attention', 'ok', 'neutral'];
const tone = (t: unknown): AugmentTone | undefined =>
  TONES.includes(t as AugmentTone) ? (t as AugmentTone) : undefined;

/** The command a list line opens. The host keeps it; the line carries only the key. */
export interface AugmentCommand {
  command: string;
  args: unknown[];
}

const asArray = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);
const obj = (v: unknown): Record<string, unknown> | undefined =>
  v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : undefined;

export function cleanCommand(command: unknown, args: unknown): AugmentCommand | undefined {
  if (typeof command !== 'string' || !COMMAND_ID.test(command)) return undefined;
  return { command, args: Array.isArray(args) ? args.slice(0, 10) : [] };
}

/**
 * Turns what a provider returned into sections. Anything that is not the schema is dropped, not
 * repaired. `keyFor` hands out a key for a list line that carries a command.
 */
export function cleanSections(
  raw: unknown,
  keyFor: (sectionId: string, cmd: AugmentCommand) => string,
): AugmentSection[] {
  const out: AugmentSection[] = [];
  const seen = new Set<string>();
  for (const rs of asArray(raw)) {
    const s = obj(rs);
    if (!s || typeof s.id !== 'string' || !ITEM_ID.test(s.id) || seen.has(s.id)) continue;
    const title = cleanText(typeof s.title === 'string' ? s.title : '', 120);
    if (!title) continue;
    seen.add(s.id);
    const blocks: AugmentBlock[] = [];
    for (const rb of asArray(s.blocks).slice(0, MAX_BLOCKS)) {
      const b = obj(rb);
      if (!b) continue;
      if (b.type === 'text' && typeof b.text === 'string') {
        blocks.push({ type: 'text', text: cleanText(b.text, 1200) });
      } else if (b.type === 'facts') {
        const rows = asArray(b.rows)
          .map(obj)
          .filter((r): r is Record<string, unknown> => !!r && typeof r.label === 'string')
          .slice(0, MAX_ROWS)
          .map((r) => {
            const t = tone(r.tone);
            return {
              label: cleanText(String(r.label), 80),
              value: cleanText(typeof r.value === 'string' ? r.value : '', 400),
              ...(t ? { tone: t } : {}),
            };
          });
        blocks.push({ type: 'facts', rows });
      } else if (b.type === 'table') {
        const columns = asArray(b.columns)
          .filter((c): c is string => typeof c === 'string')
          .slice(0, MAX_COLUMNS)
          .map((c) => cleanText(c, 80));
        if (!columns.length) continue;
        const rows = asArray(b.rows)
          .filter(Array.isArray)
          .slice(0, MAX_ROWS)
          .map((r) =>
            columns.map((_, i) => cleanText(typeof r[i] === 'string' ? (r[i] as string) : '', 400)),
          );
        blocks.push({ type: 'table', columns, rows });
      } else if (b.type === 'list') {
        const items = asArray(b.items)
          .map(obj)
          .filter((i): i is Record<string, unknown> => !!i && typeof i.label === 'string')
          .slice(0, MAX_ITEMS)
          .map((i) => {
            const cmd = cleanCommand(i.command, i.args);
            const detail = cleanOpt(i.detail, 200);
            const t = tone(i.tone);
            return {
              label: cleanText(String(i.label), 160),
              ...(detail ? { detail } : {}),
              ...(t ? { tone: t } : {}),
              ...(cmd ? { key: keyFor(s.id as string, cmd) } : {}),
            };
          });
        blocks.push({ type: 'list', items });
      }
    }
    const empty = cleanOpt(s.empty, 160);
    out.push({ id: s.id, title, blocks, ...(empty ? { empty } : {}) });
    if (out.length >= MAX_SECTIONS) break;
  }
  return out;
}
