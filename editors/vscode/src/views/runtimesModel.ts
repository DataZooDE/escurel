// The Runtimes view's rows and the routing words around them, as pure functions (no `vscode`): which
// runners are registered (#818), whether they are alive, what a skill needs from one, and which one
// holds an inbox event.
import type { EventClaim, Runtime, SkillRequires } from '../client/types';
import { formatAge, formatElapsed } from '../shared/time';
import { cleanText } from '../shared/untrustedText';

/** The statuses `list_runtimes` derives; anything newer reads as `unknown`. */
export type RuntimeStatus = 'online' | 'stale' | 'stopped' | 'unknown';

export interface RuntimeRow {
  id: string;
  label: string;
  description: string;
  tooltip: string;
  /** Picks the icon; the description says the same in words (never the icon alone). */
  status: RuntimeStatus;
  /** The runtime page: what a click opens. */
  pageId: string;
}

const STATUS_WORD: Record<RuntimeStatus, string> = {
  online: 'online',
  stale: 'stale',
  stopped: 'stopped',
  unknown: 'not reporting',
};

const statusOf = (s: string): RuntimeStatus =>
  s === 'online' || s === 'stale' || s === 'stopped' ? s : 'unknown';

const harnessWords = (r: Runtime): string =>
  r.harnesses.length
    ? r.harnesses
        .map((h) => cleanText([h.name, h.version].filter(Boolean).join(' '), 80))
        .join(', ')
    : 'no harness';

const labelWords = (r: Runtime): string =>
  r.labels.length ? r.labels.map((l) => cleanText(l, 64)).join(', ') : 'no labels';

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

function statusLine(r: Runtime, status: RuntimeStatus, now: Date): string {
  const seen = formatAge(r.last_seen, now);
  const after =
    r.stale_after_s !== null
      ? ` (stale after ${formatElapsed(r.stale_after_s * 1000)} of silence)`
      : '';
  switch (status) {
    case 'online':
      return `Online: last seen ${seen || 'recently'}${after}`;
    case 'stale':
      return `Stale: last seen ${seen || 'a while ago'}${after}. It takes no work until it reports again.`;
    case 'stopped':
      return `Stopped: it said it was shutting down${seen ? ` (${seen})` : ''}`;
    case 'unknown':
      return 'Not reporting: no status record from this runner yet';
  }
}

export function runtimeRows(runtimes: Runtime[], now: Date = new Date()): RuntimeRow[] {
  return runtimes
    .map((r) => ({ r, label: cleanText(r.runner_id, 120) }))
    .sort((a, b) => a.label.localeCompare(b.label))
    .map(({ r, label }) => {
      const status = statusOf(r.status);
      const busy = r.live_runs ? plural(r.live_runs, 'live run') : 'idle';
      return {
        id: r.page_id,
        label,
        status,
        pageId: r.page_id,
        description: [STATUS_WORD[status], harnessWords(r), labelWords(r), busy].join(' · '),
        tooltip: [
          label,
          statusLine(r, status, now),
          `Harnesses: ${harnessWords(r)}`,
          `Labels: ${labelWords(r)}`,
          ...(r.max_concurrent !== null
            ? [`At most ${plural(r.max_concurrent, 'run')} at once`]
            : []),
          `Live runs: ${r.live_runs}`,
          ...(r.version ? [`Runner ${cleanText(r.version, 60)}`] : []),
          `Page: ${r.page_id}`,
        ].join('\n'),
      };
    });
}

/** A `requires:` as a short chip and a sentence; `undefined` when nothing is required. */
export function requiresWords(
  requires: SkillRequires | undefined,
): { chip: string; detail: string } | undefined {
  if (!requires) return undefined;
  if (requires.invalid)
    return {
      chip: 'routing blocked',
      detail: 'Routing blocked: requires is malformed, so no runner takes this work',
    };
  const harness = requires.harness ? cleanText(requires.harness, 64) : undefined;
  const labels = (requires.labels ?? []).map((l) => cleanText(l, 64)).join(', ');
  if (!harness && !labels) return undefined;
  return {
    chip: `needs ${[harness, labels].filter(Boolean).join(' · ')}`,
    detail: `needs ${[harness && `harness ${harness}`, labels && `labels ${labels}`]
      .filter(Boolean)
      .join(' · ')}`,
  };
}

/**
 * Who holds an event's claim: the runtime whose `claim_holder` it is. `runtime` is absent when it
 * cannot be told: the gateway shows `claim_holder` to admins only, so a member sees that a runner
 * holds the event but not which one.
 */
export interface HeldBy {
  runtime?: string;
  /** The runtime page, when a registered runtime carries the holder. */
  pageId?: string;
  /** RFC 3339: when the lease lapses unless renewed. */
  until: string;
}

export function heldBy(claim: EventClaim | undefined, runtimes: Runtime[]): HeldBy | undefined {
  if (!claim) return undefined;
  const r = runtimes.find((x) => x.claim_holder && x.claim_holder === claim.holder);
  if (r)
    return { runtime: cleanText(r.runner_id, 120), pageId: r.page_id, until: claim.expires_at };
  // Holders are visible (an admin's read) but none matches: a runner that never registered.
  return runtimes.some((x) => x.claim_holder)
    ? { runtime: cleanText(claim.holder, 160), until: claim.expires_at }
    : { until: claim.expires_at };
}
