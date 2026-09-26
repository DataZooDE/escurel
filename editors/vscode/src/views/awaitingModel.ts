import type { Changeset, Draft, Event, Skill } from '../client';
import { pageSlug } from '../shared/pageId';
import { pluralise } from '../shared/text';

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

export type AwaitingRow = ChangesetRow | DraftRow | ConfirmGateRow;

/**
 * A changeset is decided as a whole, so the row counts its drafts rather
 * than naming one of them.
 */
export function changesetRow(changeset: Changeset): ChangesetRow {
  return {
    kind: 'changeset',
    id: changeset.changeset_id,
    label: changeset.changeset_id,
    description: `${pluralise(changeset.drafts, 'draft')} · ${changeset.author}`,
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
    description: draft.author,
    timestamp: draft.created_at ?? '',
    draft,
  };
}

/** A `confirm` skill's event waits for a human before the runner may act. */
export function confirmGateRow(event: Event): ConfirmGateRow {
  return {
    kind: 'confirm_gate',
    id: event.event_id,
    label: event.title?.trim() || event.event_id,
    description: `confirm · ${event.label_skill}`,
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

  // Human live drafts arrive with PR-1 (BACKEND_GAPS.md) — personal drafts will be merged here.

  return sortAwaitingNewestFirst(rows);
}
