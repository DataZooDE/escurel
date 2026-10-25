import * as vscode from 'vscode';
import type { FocusProfile } from '../api/types';

/** The id of the profile Escurel itself ships (`FOCUS_SETTINGS`). A companion cannot take it. */
export const BUILTIN_PROFILE = 'escurel';

const ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const KEY = /^[A-Za-z][A-Za-z0-9]*(\.[A-Za-z0-9_-]+)+$/;
/** Appearance only: the window, the workbench, breadcrumbs, the minimap and a few editor looks. */
const ALLOWED = /^(window|workbench|breadcrumbs)\.|^editor\.minimap\./;
const EDITOR_LOOKS = new Set([
  'editor.fontFamily',
  'editor.fontSize',
  'editor.lineHeight',
  'editor.lineNumbers',
  'editor.wordWrap',
  'editor.renderLineHighlight',
  'editor.cursorBlinking',
]);
/** Settings that decide what runs, where a credential goes or what is trusted are never a profile's business. */
const FORBIDDEN =
  /trust|path|command|exec|url|domain|shell|terminal|task|proxy|extension|security|auth/i;
const MAX_SETTINGS = 40;
const MAX_VALUE_CHARS = 1000;
const MAX_TOTAL_CHARS = 8000;

function isJson(v: unknown, depth = 0): boolean {
  if (depth > 4) return false;
  if (v === null || typeof v === 'boolean' || typeof v === 'string') return true;
  if (typeof v === 'number') return Number.isFinite(v);
  if (Array.isArray(v)) return v.every((x) => isJson(x, depth + 1));
  if (typeof v === 'object') {
    const proto = Object.getPrototypeOf(v) as unknown;
    if (proto !== Object.prototype && proto !== null) return false;
    return Object.entries(v as object).every(([k, x]) => k !== '__proto__' && isJson(x, depth + 1));
  }
  return false;
}

/** Checks a profile a companion offers and returns a frozen deep copy of it. */
export function validateProfile(p: FocusProfile): FocusProfile {
  if (typeof p !== 'object' || p === null)
    throw new Error('registerFocusProfile: expected an object');
  if (typeof p.id !== 'string' || !ID.test(p.id)) {
    throw new Error('registerFocusProfile: id is 1 to 64 letters, digits, ".", "_" or "-"');
  }
  if (p.id === BUILTIN_PROFILE)
    throw new Error(`registerFocusProfile: id "${BUILTIN_PROFILE}" is reserved`);
  const s = p.settings;
  if (typeof s !== 'object' || s === null || Array.isArray(s)) {
    throw new Error('registerFocusProfile: settings must be an object');
  }
  const proto = Object.getPrototypeOf(s) as unknown;
  if (proto !== Object.prototype && proto !== null) {
    throw new Error('registerFocusProfile: settings must be a plain object');
  }
  const entries = Object.entries(s);
  if (entries.length === 0) throw new Error('registerFocusProfile: settings is empty');
  if (entries.length > MAX_SETTINGS) {
    throw new Error(`registerFocusProfile: at most ${MAX_SETTINGS} settings`);
  }
  let total = 0;
  const copy: Record<string, unknown> = {};
  for (const [key, value] of entries) {
    if (
      !KEY.test(key) ||
      key === '__proto__' ||
      FORBIDDEN.test(key) ||
      !(ALLOWED.test(key) || EDITOR_LOOKS.has(key))
    ) {
      throw new Error(`registerFocusProfile: "${key}" is not a setting a focus profile may change`);
    }
    if (!isJson(value))
      throw new Error(`registerFocusProfile: the value of "${key}" is not plain data`);
    const text = JSON.stringify(value);
    total += text.length;
    if (text.length > MAX_VALUE_CHARS || total > MAX_TOTAL_CHARS) {
      throw new Error(`registerFocusProfile: the value of "${key}" is too large`);
    }
    copy[key] = JSON.parse(text) as unknown;
  }
  return Object.freeze({ id: p.id, settings: Object.freeze(copy) });
}

/** The profiles companions registered. Holds data only: no companion code runs when a profile is applied. */
export class FocusProfileRegistry {
  private readonly profiles = new Map<string, FocusProfile>();
  private readonly changed = new vscode.EventEmitter<void>();
  readonly onDidChange = this.changed.event;

  register(profile: FocusProfile): vscode.Disposable {
    const p = validateProfile(profile);
    if (this.profiles.has(p.id)) {
      throw new Error(`registerFocusProfile: a profile "${p.id}" is already registered`);
    }
    this.profiles.set(p.id, p);
    this.changed.fire();
    return {
      dispose: () => {
        if (this.profiles.get(p.id) === p) {
          this.profiles.delete(p.id);
          this.changed.fire();
        }
      },
    };
  }

  get(id: string): FocusProfile | undefined {
    return this.profiles.get(id);
  }

  ids(): string[] {
    return [...this.profiles.keys()];
  }

  dispose(): void {
    this.changed.dispose();
  }
}

/**
 * Which profile the focus toggle applies. The user's setting `escurel.focus.profile` wins when it names a
 * registered profile (or the built-in one). Unset, the one registered companion profile wins; with two or
 * none, the built-in one does: Escurel never picks between two companions on the user's behalf.
 */
export function chooseProfile(
  configured: string | undefined,
  registered: readonly string[],
): string {
  const want = (configured ?? '').trim();
  if (want === BUILTIN_PROFILE) return BUILTIN_PROFILE;
  if (want !== '' && registered.includes(want)) return want;
  return registered.length === 1 ? registered[0]! : BUILTIN_PROFILE;
}
