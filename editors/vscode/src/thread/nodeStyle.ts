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
}

export function describeNodeType(node: ThreadNode, rootEventId: string): NodeTypeDescriptor {
  let type: NodeType;
  if (node.kind === 'run') type = 'run';
  else if (node.kind === 'changeset') type = 'changeset';
  else if (node.kind === 'draft') type = 'page';
  else type = node.id === rootEventId ? 'event' : 'cascade';
  return { type, label: type, icon: type, accent: type };
}

const DECIDED = new Set(['promoted', 'discarded']);

/** Finished, with nothing left for anyone to do: it takes little room on the canvas. */
export function emphasisOf(kind: ThreadNode['kind'], state: string | null): 'compact' | 'normal' {
  if (state === null) return 'normal';
  if (kind === 'event') return state === 'processed' ? 'compact' : 'normal';
  if (kind === 'run') return state === 'processed' || state === 'cancelled' ? 'compact' : 'normal';
  return DECIDED.has(state) ? 'compact' : 'normal';
}
