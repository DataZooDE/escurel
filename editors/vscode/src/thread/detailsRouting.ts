import type { DetailsAction } from '../shared/protocol';

/** Which node of which open thread the details view is showing, and what that node offers. */
export interface Shown {
  rootEventId: string;
  nodeId: string;
  /** The page a start would run on (an instance node), if the node offers skills. */
  pageId?: string | undefined;
  /** The skills the node offers: its Skill buttons, and the skill a run executes (Fix skill). */
  skills?: readonly string[] | undefined;
  body?: string | undefined;
}

const ACTIONS = new Set<string>(['start-skill', 'view-skill', 'run-control', 'open-link', 'open-wikilink']);
const LINKS = new Set<string>(['skill', 'page', 'run', 'thread', 'review']);

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

/**
 * What the host does with a message from the details view.
 *
 * The details view is a webview of its own and can post anything, so the host acts only when it
 * names the thread whose node is being shown, that thread is still open, and the message is one of
 * the inspector actions (and a node's links). The inner message is then validated again, against that thread's
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
  // Held to the node on show: the thread would also allow any other run or page of it.
  switch (message.type) {
    case 'run-control':
      if (message.runId !== shown.nodeId || message.eventId !== undefined) return undefined;
      break;
    case 'start-skill':
      if (shown.pageId === undefined || message.pageId !== shown.pageId) return undefined;
      if (typeof message.skill !== 'string' || !shown.skills?.includes(message.skill)) {
        return undefined;
      }
      break;
    case 'open-link':
      // Held to the node on show, and to the five kinds: what each opens is the host's to decide.
      if (message.nodeId !== shown.nodeId) return undefined;
      if (typeof message.link !== 'string' || !LINKS.has(message.link)) return undefined;
      break;
    case 'open-wikilink':
      if (typeof message.wikilink !== 'string'
        || !/^\[\[(?:evolve_validation_report|evolve_experiment|plan_policy)::[^\]\s]+\]\]$/.test(message.wikilink)
        || !shown.body?.includes(message.wikilink)) {
        return undefined;
      }
      break;
    case 'view-skill':
      if (typeof message.skill !== 'string' || !shown.skills?.includes(message.skill)) {
        return undefined;
      }
      break;
  }
  return { rootEventId, message: message as unknown as DetailsAction };
}
