import { withTimeout } from '../api/contributions';
import type { AugmentCommand, PageAugmentView } from '../shared/augment';
import { cleanCommand, cleanSections } from '../shared/augment';
import { cleanText } from '../shared/untrustedText';
import { log } from '../log';
import type {
  ReviewAfterContext,
  ReviewBeforeContext,
  ReviewHook,
  ReviewPreviewContext,
} from '../api/types';

interface Disposable {
  dispose(): void;
}

export const REVIEW_HOOK_TIMEOUT_MS = 5_000;
const MEMBERS = ['before', 'after', 'preview'] as const;

/** Who hooks into the review decision. Hooks run in registration order. */
export class ReviewHookRegistry {
  private readonly hooks: ReviewHook[] = [];

  get size(): number {
    return this.hooks.length;
  }

  get hasPreview(): boolean {
    return this.hooks.some((h) => typeof h.preview === 'function');
  }

  all(): ReviewHook[] {
    return [...this.hooks];
  }

  register(hook: ReviewHook): Disposable {
    if (!hook || typeof hook !== 'object') throw new Error('registerReviewHook: hook is missing');
    const given = MEMBERS.filter((m) => hook[m] !== undefined);
    if (!given.length) {
      throw new Error('registerReviewHook: give at least one of before, after, preview');
    }
    for (const m of given) {
      if (typeof hook[m] !== 'function') {
        throw new Error(`registerReviewHook: ${m} must be a function`);
      }
    }
    this.hooks.push(hook);
    return {
      dispose: () => {
        const at = this.hooks.indexOf(hook);
        if (at >= 0) this.hooks.splice(at, 1);
      },
    };
  }
}

export interface BeforeOutcome {
  /** The strictest note any hook asked for. */
  note: 'none' | 'optional' | 'required';
  prompt?: string;
  /** The first veto. Only this blocks a decision. */
  veto?: string;
}

const RANK = { none: 0, optional: 1, required: 2 } as const;

/**
 * Asks every `before` hook. What comes back from a hook is another extension's data: a hook that throws,
 * times out or answers nonsense counts as no answer, so a faulty companion can never stop a decision;
 * only a non-empty `veto` string does.
 */
export async function collectBefore(
  registry: ReviewHookRegistry,
  ctx: Omit<ReviewBeforeContext, 'signal'>,
  timeoutMs = REVIEW_HOOK_TIMEOUT_MS,
): Promise<BeforeOutcome> {
  const out: BeforeOutcome = { note: 'none' };
  for (const hook of registry.all()) {
    if (!hook.before) continue;
    let r: unknown;
    try {
      r = await withTimeout(async (signal) => hook.before!({ ...ctx, signal }), timeoutMs);
    } catch (err) {
      log().warn(`escurel: review hook (before) failed: ${String(err)}`);
      continue;
    }
    if (!r || typeof r !== 'object') continue;
    const res = r as Record<string, unknown>;
    if (typeof res.veto === 'string' && cleanText(res.veto, 300) && out.veto === undefined) {
      out.veto = cleanText(res.veto, 300);
    }
    if ((res.note === 'required' || res.note === 'optional') && RANK[res.note] > RANK[out.note]) {
      out.note = res.note;
      delete out.prompt;
    }
    if (
      res.note === out.note &&
      out.prompt === undefined &&
      typeof res.notePrompt === 'string' &&
      cleanText(res.notePrompt, 200)
    ) {
      out.prompt = cleanText(res.notePrompt, 200);
    }
  }
  return out;
}

/** Tells every `after` hook what was decided. Never throws, never waits longer than the timeout per hook. */
export async function runAfter(
  registry: ReviewHookRegistry,
  rec: ReviewAfterContext,
  timeoutMs = REVIEW_HOOK_TIMEOUT_MS,
): Promise<void> {
  const provenance: Record<string, string> = {};
  for (const [k, v] of Object.entries(rec.provenance ?? {})) {
    if (typeof v === 'string') provenance[k] = v;
  }
  for (const hook of registry.all()) {
    if (!hook.after) continue;
    try {
      await withTimeout(async () => hook.after!({ ...rec, provenance }), timeoutMs);
    } catch (err) {
      log().warn(`escurel: review hook (after) failed: ${String(err)}`);
    }
  }
}

export interface CollectedPreview {
  view: PageAugmentView;
  /** Handle -> command; the webview only ever names the handle. */
  commands: Map<string, AugmentCommand>;
}

/** The sections every `preview` hook adds beside a draft's diff, cleaned; undefined when there are none. */
export async function collectPreview(
  registry: ReviewHookRegistry,
  ctx: Omit<ReviewPreviewContext, 'signal'>,
  timeoutMs = REVIEW_HOOK_TIMEOUT_MS,
): Promise<CollectedPreview | undefined> {
  const view: PageAugmentView = { sections: [], actions: [], forms: [] };
  const commands = new Map<string, AugmentCommand>();
  let n = 0;
  for (const hook of registry.all()) {
    if (!hook.preview) continue;
    try {
      const raw = await withTimeout(async (signal) => hook.preview!({ ...ctx, signal }), timeoutMs);
      const seen = new Set(view.sections.map((s) => s.id));
      for (const s of cleanSections(raw, (_sid, cmd) => {
        const key = `p:${(n += 1)}`;
        commands.set(key, cleanCommand(cmd.command, cmd.args)!);
        return key;
      })) {
        if (!seen.has(s.id) && view.sections.length < 12) view.sections.push(s);
      }
    } catch (err) {
      log().warn(`escurel: review hook (preview) failed: ${String(err)}`);
      view.unavailable = true;
    }
  }
  return view.sections.length || view.unavailable ? { view, commands } : undefined;
}
