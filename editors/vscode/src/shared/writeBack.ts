import type { Event } from '../client/types';

// Write-back: a change to a row of a remote source is PROPOSED as a draft carrying a reserved
// `write_back` block; a human promotes it; only then does the gateway change the source. These are the
// pure parts the extension needs: build the proposal, read the outcome events, say it in words.

export interface Proposal {
  pageId: string;
  skill: string;
  /** The writable column to change (a frontmatter field name). */
  field: string;
  value: string | number | boolean;
  /** `backend_projection.etag` of the row as the person saw it: the change is only applied to THIS state. */
  baseEtag: string;
  notes: string;
}

const instanceId = (pageId: string): string =>
  pageId
    .replace(/^markdown\/instances\//, '')
    .replace(/\.md$/, '')
    .split('/')
    .pop() ?? pageId;

/** A YAML scalar that cannot break out of its line: strings are JSON-quoted (valid YAML), others verbatim. */
const scalar = (v: string | number | boolean): string =>
  typeof v === 'string' ? JSON.stringify(v) : String(v);

/** The draft's markdown: the person's notes plus the reserved `write_back` block. */
export function buildProposal(p: Proposal): string {
  return (
    '---\n' +
    'kind: instance\n' +
    `id: ${instanceId(p.pageId)}\n` +
    `skill: ${p.skill}\n` +
    'write_back:\n' +
    `  patch: { ${p.field}: ${scalar(p.value)} }\n` +
    `  base_etag: ${JSON.stringify(p.baseEtag)}\n` +
    '---\n' +
    `${p.notes}\n`
  );
}

export type WriteBackOutcome = 'applying' | 'applied' | 'failed' | 'rejected' | 'conflict';

export interface WriteBackStatus {
  outcome: WriteBackOutcome;
  at: string;
  draftId: string;
  attempts: number;
}

const OUTCOMES: readonly WriteBackOutcome[] = [
  'applying',
  'applied',
  'failed',
  'rejected',
  'conflict',
];

/** The newest write-back outcome among a page's events (`escurel:write-back`, system), if any. */
export function latestWriteBack(events: readonly Event[]): WriteBackStatus | undefined {
  let best: WriteBackStatus | undefined;
  for (const e of events) {
    if (e.label_skill !== 'escurel:write-back' || !e.body) continue;
    let body: unknown;
    try {
      body = JSON.parse(e.body);
    } catch {
      continue;
    }
    if (typeof body !== 'object' || body === null) continue;
    const b = body as Record<string, unknown>;
    if (!OUTCOMES.includes(b.outcome as WriteBackOutcome) || typeof b.draft_id !== 'string')
      continue;
    const at = e.at ?? '';
    // `applying` is written just before the outcome, in the same instant: on a tie the outcome wins,
    // so a finished change is never shown as still being sent, whichever way the events are ordered.
    const outranks =
      !best ||
      at > best.at ||
      (at === best.at && (best.outcome === 'applying' || b.outcome !== 'applying'));
    if (outranks)
      best = {
        outcome: b.outcome as WriteBackOutcome,
        at,
        draftId: b.draft_id,
        attempts: typeof b.attempts === 'number' ? b.attempts : 0,
      };
  }
  return best;
}

const hhmm = (at: string): string => at.slice(11, 16);

/** What the last write-back did, in words a person can act on. */
export function writeBackLine(s: WriteBackStatus): string {
  const t = `${hhmm(s.at)} UTC`;
  switch (s.outcome) {
    case 'applying':
      return `A change is being sent to the source (since ${t}).`;
    case 'applied':
      return `Last change sent to the source at ${t}: applied.`;
    case 'failed':
      if (s.attempts === 0)
        return `Last change to the source at ${t} did not go through: the source could not be reached, so nothing was sent. Promote it again from Awaiting You to retry.`;
      return `Last change to the source at ${t} could not be sent after ${s.attempts} attempts. Promote it again from Awaiting You to retry.`;
    case 'rejected':
      return `Last change to the source at ${t} was rejected by it. Propose it again with a different value.`;
    case 'conflict':
      return `Last change to the source at ${t} conflicted: the row had changed. Read it again and propose again.`;
  }
}

/** Plain advice for the gateway's write-back refusal codes; `undefined` for any other code. */
export function describeWriteBackRefusal(code: string, message: string): string | undefined {
  switch (code) {
    case 'write_back_conflict':
      return 'The row has changed upstream since this change was proposed. Read it again and propose again.';
    case 'write_back_failed':
      return `Write-back failed: ${message} The draft stays open; promote it again to retry.`;
    case 'write_back_rejected':
      return `Write-back was rejected by the source: ${message}`;
    case 'write_back_unknown_outcome':
      return 'An earlier attempt may have reached the source and its outcome is unknown. It is not repeated automatically: check the source, then discard the draft and propose again.';
    case 'backend_read_only_field':
      return `${message} Only the columns the skill marks writable can be changed in the source.`;
    default:
      return undefined;
  }
}

const MAX_VALUE = 2000;

/** What the person typed, as the value for a field: it keeps the type the field's CURRENT value has. */
export function parseProposedValue(
  raw: string,
  current: unknown,
): { ok: true; value: string | number | boolean } | { ok: false; error: string } {
  const text = raw.trim();
  if (text === '') return { ok: false, error: 'Enter a value.' };
  if (text.length > MAX_VALUE)
    return { ok: false, error: `That is too long (limit ${MAX_VALUE} characters).` };
  if (typeof current === 'number') {
    const n = Number(text);
    return Number.isFinite(n) ? { ok: true, value: n } : { ok: false, error: 'Enter a number.' };
  }
  if (typeof current === 'boolean') {
    const v = text.toLowerCase();
    if (['true', 'yes', '1'].includes(v)) return { ok: true, value: true };
    if (['false', 'no', '0'].includes(v)) return { ok: true, value: false };
    return { ok: false, error: 'Enter true or false.' };
  }
  return { ok: true, value: text };
}
