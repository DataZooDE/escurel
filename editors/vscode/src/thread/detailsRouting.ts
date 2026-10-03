import type { DetailsAction } from '../shared/protocol';

/** Which node of which open thread the details view is showing. */
export interface Shown {
  rootEventId: string;
  nodeId: string;
}

const ACTIONS = new Set<string>(['start-skill', 'view-skill', 'run-control']);

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/**
 * What the host does with a message from the details view.
 *
 * The details view is a webview of its own and can post anything, so the host acts only when it
 * names the thread whose node is being shown, that thread is still open, and the message is one of
 * the three inspector actions. The inner message is then validated again, against that thread's
 * loaded state, by `resolveThreadAction`: an id in it is never trusted.
 */
export function acceptDetailsAction(
  shown: Shown | undefined,
  isOpen: (rootEventId: string) => boolean,
  raw: unknown,
): { rootEventId: string; message: DetailsAction } | undefined {
  if (!shown || !isRecord(raw) || raw.type !== 'details-action') return undefined;
  const { rootEventId, message } = raw;
  if (typeof rootEventId !== 'string' || rootEventId !== shown.rootEventId) return undefined;
  if (!isOpen(rootEventId)) return undefined;
  if (!isRecord(message) || typeof message.type !== 'string' || !ACTIONS.has(message.type)) {
    return undefined;
  }
  return { rootEventId, message: message as unknown as DetailsAction };
}

/** After a thread reloads: keep showing the node if it is still in the thread, else nothing. */
export function shownAfterReload(
  shown: Shown | undefined,
  rootEventId: string,
  nodeIds: readonly string[],
): Shown | undefined {
  if (!shown || shown.rootEventId !== rootEventId) return shown;
  return nodeIds.includes(shown.nodeId) ? shown : undefined;
}

/** After a thread panel closes: nothing to show for it any more. */
export function shownAfterClose(shown: Shown | undefined, rootEventId: string): Shown | undefined {
  return shown && shown.rootEventId === rootEventId ? undefined : shown;
}
