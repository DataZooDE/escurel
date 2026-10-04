// How a row of "Awaiting you" reads on screen: ONE structure for every kind of row,
// "<what> — <kind>" with who and when after it. The model rows keep their ids and raw fields (the
// commands address them); this is only the words.
import type { AwaitingRow } from './awaitingModel';
import { shortAgo } from './runsModel';

/** "agent:supplier-risk" -> "supplier-risk agent"; a person stays as they are. */
export function whoWords(author: string | null | undefined): string {
  const a = (author ?? '').trim();
  if (!a) return '';
  const m = /^(?:agent|agt):(.+)$/.exec(a);
  return m ? `${m[1]} agent` : a;
}

export interface AwaitingDisplay {
  label: string;
  description: string;
  /** What kind of thing it is, for the tooltip and the accessible name. */
  kind: string;
}

const join = (...parts: (string | undefined)[]): string => parts.filter(Boolean).join(' · ');

export function awaitingDisplay(row: AwaitingRow, nowMs: number): AwaitingDisplay {
  const ago = row.timestamp ? shortAgo(row.timestamp, nowMs) : '';
  switch (row.kind) {
    case 'changeset': {
      const n = row.changeset.drafts;
      return {
        label: `${row.label} — Proposed changes`,
        description: join(
          `${n} ${n === 1 ? 'change' : 'changes'}`,
          whoWords(row.changeset.author),
          ago,
        ),
        kind: 'Proposed changes: decide on them together',
      };
    }
    case 'draft':
      return {
        label: `${row.label} — Proposed change`,
        description: join(whoWords(row.draft.author), ago),
        kind: 'A proposed change to this page',
      };
    case 'confirm_gate':
      return {
        label: `${row.label} — Needs your confirmation`,
        description: join(row.event.label_skill, ago),
        kind: 'The agent waits for you to confirm before it acts',
      };
    case 'plan': {
      const subject = row.pageId ? (row.label.split(' on ').at(-1) ?? row.label) : row.label;
      return {
        label: `${row.skill && row.pageId ? subject : row.label.replace(/^Plan ready · /, '')} — Plan to approve`,
        description: join(row.skill, ago),
        kind: 'A plan the agent wrote and stopped at. Nothing runs until you approve it',
      };
    }
  }
}
