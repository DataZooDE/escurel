import type { NodeTarget, ThreadView } from '../shared/protocol';

/**
 * The command that owns the surface a node opens. Thread nodes never open anything
 * themselves: each surface already has one command, and routing through it keeps every
 * entry point (Inbox row, Threads outline, canvas click, page strip) landing in the same place.
 */
export function commandForTarget(
  target: NodeTarget,
): { command: string; args: unknown[] } | undefined {
  switch (target.open) {
    case 'thread':
      return { command: 'escurel.openThread', args: [target.rootEventId] };
    case 'run':
      return { command: 'escurel.openRun', args: [target.runId] };
    case 'page':
      return { command: 'escurel.openInstance', args: [target.pageId] };
    case 'review':
      // The shape `resolveReviewTarget` reads. A draft also carries its changeset id, so a
      // draft target sends ONLY the draft id: anything more reopens the M2 defect where a
      // draft in a changeset opened the changeset picker.
      return {
        command: 'escurel.openReview',
        args: [target.draftId ? { draftId: target.draftId } : { changesetId: target.changesetId }],
      };
    case 'nothing':
      return undefined;
  }
}

/** The thread root an argument names: a bare id, or an event row from the Inbox. */
export function rootEventIdOf(arg: unknown): string | undefined {
  if (typeof arg === 'string') return arg || undefined;
  if (arg && typeof arg === 'object') {
    const o = arg as { root_event_id?: unknown; event_id?: unknown };
    if (typeof o.root_event_id === 'string' && o.root_event_id) return o.root_event_id;
    // A root event carries no root id of its own: it IS the root.
    if (typeof o.event_id === 'string' && o.event_id) return o.event_id;
  }
  return undefined;
}

/**
 * The review command argument a gate message is allowed to reach, or `undefined`.
 *
 * A message from a webview is input, however well-behaved the webview is, and Promote writes.
 * So the ids are checked against the thread the HOST loaded: the node must exist, be of the
 * kind the id claims, and still carry an open gate (a decided node has none). Exactly one id
 * is accepted — a message naming both used to select the changeset silently.
 */
export function resolveGate(
  view: ThreadView,
  msg: { changesetId?: string; draftId?: string },
): { changesetId: string } | { draftId: string } | undefined {
  if (Boolean(msg.changesetId) === Boolean(msg.draftId)) return undefined;
  const id = msg.changesetId ?? msg.draftId!;
  const node = view.nodes.find((n) => n.id === id);
  if (!node?.gate) return undefined;
  if (msg.changesetId) return node.kind === 'changeset' ? { changesetId: id } : undefined;
  return node.kind === 'draft' ? { draftId: id } : undefined;
}

/** A node of the thread the host loaded. The webview is not trusted to name one that exists. */
export function knownNodeId(view: ThreadView | undefined, id: unknown): boolean {
  return typeof id === 'string' && Boolean(view?.nodes.some((n) => n.id === id));
}

/** A node that is part of the loaded thread AND has something to collapse. */
export function collapsibleNodeId(view: ThreadView | undefined, id: unknown): boolean {
  return typeof id === 'string' && Boolean(view?.nodes.find((n) => n.id === id)?.collapsible);
}
