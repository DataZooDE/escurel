import { pageSlug } from '../shared/pageId';
import type { ThreadNode } from '../shared/protocol';

/** The kinds of card a person tells apart. A cascade event is not the same thing as the root event. */
export type NodeType = 'event' | 'cascade' | 'run' | 'changeset' | 'page';

export interface NodeTypeDescriptor {
  type: NodeType;
  /** The word on the card, so the type is never carried by colour alone. */
  label: string;
  /** Which inline icon the webview draws. */
  icon: NodeType;
  /** Which theme colour the accent bar uses; the webview maps it to a --vscode-* token. */
  accent: NodeType;
  /**
   * What tells two cards of one type apart: the SKILL a page belongs to (an order is not an analysis),
   * the page a run worked on. Absent when the title already says it.
   */
  qualifier?: string;
}

export function describeNodeType(node: ThreadNode, rootEventId: string): NodeTypeDescriptor {
  let type: NodeType;
  if (node.kind === 'run') type = 'run';
  else if (node.kind === 'changeset') type = 'changeset';
  else if (node.kind === 'draft') type = 'page';
  else type = node.id === rootEventId ? 'event' : 'cascade';
  const qualifier =
    type === 'page'
      ? node.skill
      : type === 'run' && node.pageId
        ? `on ${pageSlug(node.pageId, node.skill)}`
        : undefined;
  return { type, label: type, icon: type, accent: type, ...(qualifier ? { qualifier } : {}) };
}

const DECIDED = new Set(['promoted', 'discarded']);

/** Finished, with nothing left for anyone to do: it takes little room on the canvas. */
export type NeedsYou = NonNullable<ThreadNode['needsYou']>;

/**
 * Why a node waits on a person, or undefined. An open changeset is decided as a whole, so the drafts
 * inside it carry no mark of their own; a draft in no changeset (a live human draft) does.
 */
export function needsYouOf(
  kind: ThreadNode['kind'],
  state: string | null,
  inChangeset: boolean,
): NeedsYou | undefined {
  if (kind === 'changeset' && state === 'open') return { reason: 'review', text: 'Review changes' };
  if (kind === 'draft' && state === 'open' && !inChangeset)
    return { reason: 'review', text: 'Review changes' };
  if (kind !== 'run') return undefined;
  if (state === 'planned') return { reason: 'approve-plan', text: 'Approve the plan' };
  if (state === 'failed') return { reason: 'failed', text: 'Run failed' };
  if (state === 'dead_letter') return { reason: 'failed', text: 'Run dead-lettered' };
  // Not in the lineage today (it is a workflow operation status); kept so it lights up the day it is.
  if (state === 'awaiting_human') return { reason: 'ask-human', text: 'Waiting for your answer' };
  return undefined;
}

export function emphasisOf(kind: ThreadNode['kind'], state: string | null): 'compact' | 'normal' {
  if (state === null) return 'normal';
  if (kind === 'event') return state === 'processed' ? 'compact' : 'normal';
  if (kind === 'run') return state === 'processed' || state === 'cancelled' ? 'compact' : 'normal';
  return DECIDED.has(state) ? 'compact' : 'normal';
}
