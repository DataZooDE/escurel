import type { Event } from '../client/types';
import { cleanText } from './untrustedText';

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

/**
 * A YAML double-quoted scalar. JSON.stringify output is valid YAML EXCEPT that YAML treats U+0085, U+2028
 * and U+2029 as line breaks even inside quotes (they fold to a space and change the value), and
 * JSON.stringify leaves them raw. They are written as \\u escapes, which YAML reads back to the character.
 */
const quote = (s: string): string =>
  JSON.stringify(s).replace(
    /[\u0085\u2028\u2029]/g,
    (c) => `\\u${c.charCodeAt(0).toString(16).padStart(4, '0')}`,
  );

/** A YAML scalar that cannot break out of its line: strings are quoted, others verbatim. */
const scalar = (v: string | number | boolean): string =>
  typeof v === 'string' ? quote(v) : String(v);

/** The row's current value as shown in a prompt: it comes from the source, so it is bounded and cleaned. */
export function describeCurrent(current: unknown): string {
  if (current === undefined || current === null || current === '') return '(empty)';
  if (typeof current === 'string') return cleanText(current, 120);
  return cleanText(JSON.stringify(current) ?? String(current), 120);
}

/** A column name from the gateway. Only a plain identifier is put into the YAML key position. */
const COLUMN = /^[A-Za-z0-9_.-]+$/;

/** The draft's markdown: the person's notes plus the reserved `write_back` block. */
export function buildProposal(p: Proposal): string {
  // Every name here comes from the gateway: a hostile column such as `a, b: x` must not add a key to the
  // reserved block, and a skill or id with a newline must not add a line. Quote or refuse.
  if (!COLUMN.test(p.field))
    throw new Error(
      `refusing to propose a change to column ${JSON.stringify(p.field)}: not a plain column name`,
    );
  return (
    '---\n' +
    'kind: instance\n' +
    `id: ${quote(instanceId(p.pageId))}\n` +
    `skill: ${quote(p.skill)}\n` +
    'write_back:\n' +
    `  patch: { ${quote(p.field)}: ${scalar(p.value)} }\n` +
    `  base_etag: ${quote(p.baseEtag)}\n` +
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
  /** The column's declared kind (`int`, `float`, `bool`, …) for a column whose current value is empty. */
  kind?: string,
): { ok: true; value: string | number | boolean } | { ok: false; error: string } {
  const text = raw.trim();
  if (text === '') return { ok: false, error: 'Enter a value.' };
  if (text.length > MAX_VALUE)
    return { ok: false, error: `That is too long (limit ${MAX_VALUE} characters).` };
  // An empty column has no current value to take its type from; the skill's declared kind says it.
  const empty = current === undefined || current === null || current === '';
  const asNumber = typeof current === 'number' || (empty && (kind === 'int' || kind === 'float'));
  const asBoolean = typeof current === 'boolean' || (empty && kind === 'bool');
  if (asNumber) {
    const n = Number(text);
    return Number.isFinite(n) ? { ok: true, value: n } : { ok: false, error: 'Enter a number.' };
  }
  if (asBoolean) {
    const v = text.toLowerCase();
    if (['true', 'yes', '1'].includes(v)) return { ok: true, value: true };
    if (['false', 'no', '0'].includes(v)) return { ok: true, value: false };
    return { ok: false, error: 'Enter true or false.' };
  }
  return { ok: true, value: text };
}
