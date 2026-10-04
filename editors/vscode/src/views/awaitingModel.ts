import type { Changeset, Draft, Event, Skill } from '../client';
import { pageSlug } from '../shared/pageId';
import { cleanText } from '../shared/untrustedText';
import { pluralise } from '../shared/text';
import { planRows, type PlanRow } from './planRows';

export type AwaitingKind = 'changeset' | 'draft' | 'confirm_gate';

export interface ChangesetRow {
  kind: 'changeset';
  id: string;
  label: string;
  description: string;
  timestamp: string;
  changeset: Changeset;
}

export interface DraftRow {
  kind: 'draft';
  id: string;
  label: string;
  description: string;
  timestamp: string;
  draft: Draft;
}

export interface ConfirmGateRow {
  kind: 'confirm_gate';
  id: string;
  label: string;
  description: string;
  timestamp: string;
  event: Event;
}

export type AwaitingRow = ChangesetRow | DraftRow | ConfirmGateRow | PlanRow;

/**
 * What a reviewer recognises a changeset by: the page it changes (and how many more), not its
 * ULID. The id stays on the row, because promote and discard are addressed by it.
 */
function changesetLabel(changeset: Changeset): string {
  const [first, ...rest] = changeset.target_page_ids;
  if (!first) return changeset.changeset_id;
  return rest.length > 0 ? `${pageSlug(first)} +${rest.length}` : pageSlug(first);
}

/**
 * A changeset is decided as a whole, so the row counts its drafts rather
 * than naming one of them.
 */
export function changesetRow(changeset: Changeset): ChangesetRow {
  return {
    kind: 'changeset',
    id: changeset.changeset_id,
    label: changesetLabel(changeset),
    description: `${pluralise(changeset.drafts, 'draft')} · ${cleanText(changeset.author ?? '', 80)}`,
    timestamp: changeset.created_at ?? '',
    changeset,
  };
}

/**
 * A draft with no changeset is decided on its own; the page it targets is
 * what a reviewer recognises it by. A draft row knows no skill, so the slug
 * falls back to the first `__`.
 */
export function draftRow(draft: Draft): DraftRow {
  return {
    kind: 'draft',
    id: draft.draft_id,
    label: pageSlug(draft.target_page_id),
    description: cleanText(draft.author ?? '', 80),
    timestamp: draft.created_at ?? '',
    draft,
  };
}

/** A `confirm` skill's event waits for a human before the runner may act. */
export function confirmGateRow(event: Event): ConfirmGateRow {
  return {
    kind: 'confirm_gate',
    id: event.event_id,
    label: cleanText(event.title ?? '', 160) || event.event_id,
    description: `confirm · ${cleanText(event.label_skill, 80)}`,
    timestamp: event.at ?? '',
    event,
  };
}

/**
 * Only an explicit `autonomy: confirm` gates. An ABSENT autonomy means
 * unset OR unrecognised (see `knowledgeModel`), which reads as review — a
 * queue that guessed `confirm` from absence would ask for a decision
 * nobody's skill asked for.
 */
export function isConfirmGate(event: Event, skillMap: Map<string, Skill>): boolean {
  if (event.status !== 'inbox') return false;
  const skill = skillMap.get(event.label_skill);
  // An absent autonomy is not confirm (absent means review).
  return skill?.autonomy === 'confirm';
}

/** Newest first, with the id breaking ties so the order is stable. */
export function sortAwaitingNewestFirst(rows: AwaitingRow[]): AwaitingRow[] {
  return [...rows].sort((a, b) => {
    const timeA = a.timestamp ? new Date(a.timestamp).getTime() : 0;
    const timeB = b.timestamp ? new Date(b.timestamp).getTime() : 0;
    if (timeA !== timeB) return timeB - timeA;
    return b.id.localeCompare(a.id);
  });
}

export interface AwaitingInputs {
  changesets: Changeset[];
  drafts: Draft[];
  events: Event[];
  skills: Skill[];
  /** `escurel:run` lifecycle events (to find plans waiting for approval). */
  runEvents?: Event[];
  /** User-kind events of any status (the triggers and the approvals of plans). */
  userEvents?: Event[];
}

/**
 * The three things that wait on a human, in one queue (SPEC §3.2). The
 * reads are filtered here rather than on the wire because neither
 * `list_changesets` nor `list_drafts` takes a status argument.
 */
export function buildAwaitingRows(inputs: AwaitingInputs): AwaitingRow[] {
  const rows: AwaitingRow[] = [];

  for (const cs of inputs.changesets) {
    if (cs.status === 'open') {
      rows.push(changesetRow(cs));
    }
  }

  for (const draft of inputs.drafts) {
    if (
      draft.status === 'open' &&
      (draft.changeset_id === null || draft.changeset_id === undefined)
    ) {
      rows.push(draftRow(draft));
    }
  }

  const skillMap = new Map<string, Skill>(inputs.skills.map((s) => [s.id, s]));
  for (const event of inputs.events) {
    if (isConfirmGate(event, skillMap)) {
      rows.push(confirmGateRow(event));
    }
  }

  // A plan that ended `planned` and that nobody approved waits on a human too.
  rows.push(...planRows(inputs.runEvents ?? [], inputs.userEvents ?? []));

  // Human live drafts arrive with PR-1 (BACKEND_GAPS.md) — personal drafts will be merged here.

  return sortAwaitingNewestFirst(rows);
}

/**
 * What a screen reader announces for a row, in place of the visible text. VS Code falls back to the
 * tooltip, which carried the changeset's ULID, so a reviewer heard an id where the screen shows the
 * order. This says what the row is ABOUT.
 */
export function accessibleLabel(row: AwaitingRow): string {
  switch (row.kind) {
    case 'changeset':
      return `Changeset for ${row.label}, ${row.description}`;
    case 'draft':
      return `Draft for ${row.label}, ${row.description}`;
    case 'confirm_gate':
      return `Waiting for you: ${row.label}, ${row.description}`;
    case 'plan':
      return `${row.label}. Waiting for you to approve it.`;
  }
}
