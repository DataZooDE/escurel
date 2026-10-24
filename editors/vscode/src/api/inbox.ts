import type { InboxRow } from '../views/inboxModel';
import { log } from '../log';
import { cleanOpt, cleanText } from '../shared/untrustedText';
import { withTimeout } from './contributions';
import type {
  InboxDecoration,
  InboxDecorator,
  InboxRowData,
  InboxRowStatus,
} from './types';

interface Disposable {
  dispose(): void;
}

export const INBOX_DECORATE_TIMEOUT_MS = 2_000;
const SKILL_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
const STATUSES: readonly InboxRowStatus[] = ['open', 'answered', 'failed', 'done'];
const DECIDED: readonly InboxRowStatus[] = ['answered', 'failed', 'done'];

/** Who decorates the Inbox rows of which skill; several companions may decorate one skill. */
export class InboxDecoratorRegistry {
  private readonly entries = new Map<string, InboxDecorator[]>();
  private readonly all: InboxDecorator[] = [];
  private readonly listeners = new Set<() => void>();

  onDidChange(listener: () => void): Disposable {
    this.listeners.add(listener);
    return { dispose: () => void this.listeners.delete(listener) };
  }

  private changed(): void {
    for (const l of [...this.listeners]) l();
  }

  get size(): number {
    return this.all.length;
  }

  for(skill: string): InboxDecorator[] {
    return [...(this.entries.get(skill) ?? [])];
  }

  /** Every decorator in registration order (for the empty message). */
  list(): InboxDecorator[] {
    return [...this.all];
  }

  register(skill: string, decorator: InboxDecorator): Disposable {
    if (typeof skill !== 'string' || !SKILL_ID.test(skill)) {
      throw new Error('registerInboxDecorator: invalid skill id');
    }
    if (!decorator || typeof decorator !== 'object') {
      throw new Error('registerInboxDecorator: decorator is missing');
    }
    if (decorator.decorate === undefined && decorator.emptyMessage === undefined) {
      throw new Error('registerInboxDecorator: give at least one of decorate, emptyMessage');
    }
    if (decorator.decorate !== undefined && typeof decorator.decorate !== 'function') {
      throw new Error('registerInboxDecorator: decorate must be a function');
    }
    const m = decorator.emptyMessage;
    if (m !== undefined && typeof m !== 'string' && (!m || typeof m !== 'object' || Array.isArray(m))) {
      throw new Error('registerInboxDecorator: emptyMessage must be a string or one text per language');
    }
    const list = this.entries.get(skill) ?? [];
    list.push(decorator);
    this.entries.set(skill, list);
    this.all.push(decorator);
    this.changed();
    return {
      dispose: () => {
        for (const arr of [list, this.all]) {
          const at = arr.indexOf(decorator);
          if (at >= 0) arr.splice(at, 1);
        }
        if (!list.length) this.entries.delete(skill);
        this.changed();
      },
    };
  }
}

const str = (v: unknown): string => (typeof v === 'string' ? v : '');

function dataOf(row: InboxRow): InboxRowData {
  const e = row.event;
  const provenance: Record<string, string> = {};
  for (const [k, v] of Object.entries(e.provenance ?? {})) {
    if (typeof v === 'string') provenance[k] = v;
  }
  return {
    eventId: e.event_id,
    labelSkill: e.label_skill,
    title: str(e.title),
    body: str(e.body),
    pageId: str(e.instance_page_id),
    at: str(e.at),
    rootEventId: str(e.root_event_id),
    runId: str(e.run_id),
    eventStatus: str(e.status),
    provenance,
  };
}

function cleanDecoration(raw: unknown): InboxDecoration {
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>;
  const out: InboxDecoration = {};
  const badge = cleanOpt(r.badge, 24);
  if (badge) out.badge = badge;
  const description = cleanOpt(r.description, 160);
  if (description) out.description = description;
  if (STATUSES.includes(r.status as InboxRowStatus)) out.status = r.status as InboxRowStatus;
  if (r.hidden === true) out.hidden = true;
  return out;
}

export interface DecoratedRows {
  rows: InboxRow[];
  /** Rows whose status is `open`. */
  open: number;
}

/**
 * Applies the decorators to the rows the Inbox would show. Decoration is data from another extension: it
 * is cleaned and bounded, the first decorator that sets a field wins, and a decorator that fails costs
 * only its own rows their decoration.
 */
export async function decorateRows(
  registry: InboxDecoratorRegistry,
  rows: InboxRow[],
  _now?: number,
  timeoutMs = INBOX_DECORATE_TIMEOUT_MS,
): Promise<DecoratedRows> {
  if (!registry.size) return { rows, open: 0 };
  const data = rows.map(dataOf);
  const out: (InboxRow | undefined)[] = await Promise.all(
    rows.map(async (row, i) => {
      const decorators = registry.for(row.event.label_skill).filter((d) => d.decorate);
      if (!decorators.length) return row;
      const merged: InboxDecoration = {};
      for (const d of decorators) {
        try {
          const got = cleanDecoration(
            await withTimeout(async () => d.decorate!(data[i]!, data), timeoutMs),
          );
          for (const k of Object.keys(got) as (keyof InboxDecoration)[]) {
            if (merged[k] === undefined) (merged as Record<string, unknown>)[k] = got[k];
          }
        } catch (err) {
          log().warn(`escurel: inbox decorator failed: ${String(err)}`);
        }
      }
      if (merged.hidden) return undefined;
      const secondary = merged.description ?? row.description;
      const description = [merged.badge, secondary].filter(Boolean).join(' · ');
      return {
        ...row,
        description,
        ...(merged.badge ? { badge: merged.badge } : {}),
        ...(merged.status ? { status: merged.status } : {}),
      };
    }),
  );
  const kept = out.filter((r): r is InboxRow => !!r);
  // A decided row (answered, failed, done) goes after the ones that still wait; the order inside each stays.
  const waiting = kept.filter((r) => !r.status || !DECIDED.includes(r.status));
  const decided = kept.filter((r) => r.status && DECIDED.includes(r.status));
  return { rows: [...waiting, ...decided], open: kept.filter((r) => r.status === 'open').length };
}

/** The empty-state message for the window's language: exact, then the language family, then English. */
export function emptyInboxMessage(registry: InboxDecoratorRegistry, language: string): string | undefined {
  for (const d of registry.list()) {
    const m = d.emptyMessage;
    if (m === undefined) continue;
    if (typeof m === 'string') return cleanText(m, 200) || undefined;
    const lang = language.toLowerCase();
    const byKey = new Map(Object.entries(m).map(([k, v]) => [k.toLowerCase(), v]));
    const pick =
      byKey.get(lang) ?? byKey.get(lang.split('-')[0]!) ?? byKey.get('en') ?? [...byKey.values()][0];
    const text = typeof pick === 'string' ? cleanText(pick, 200) : '';
    if (text) return text;
  }
  return undefined;
}
