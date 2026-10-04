import { pageSlug } from '../shared/pageId';
import {
  NODE_LINK_KINDS,
  type NodeLink,
  type NodeLinkKind,
  type ThreadNode,
  type ThreadView,
} from '../shared/protocol';
import { commandForTarget } from './nodeTarget';

/**
 * Where a node leads, in a person's words. A card says what a node IS; these say what to do next:
 * read the skill that produced it, open the page it is about, the run behind it, the thread it
 * belongs to, or the review that still waits on you.
 */
export function nodeLinks(node: ThreadNode, rootEventId: string): NodeLink[] {
  const links: NodeLink[] = [];
  if (node.skill) links.push({ id: 'skill', label: `View skill: ${node.skill}` });
  // A page that was only proposed does not exist yet: it has a review, not a page to open.
  const pageExists =
    node.kind === 'draft' ? node.state === 'promoted' : node.kind !== 'changeset' && !!node.pageId;
  if (node.pageId && pageExists) {
    links.push({ id: 'page', label: `Open page: ${pageSlug(node.pageId, node.skill)}` });
  }
  // The run behind a node: a run's own detail, or the run that proposed or wrote a change.
  if (node.kind === 'run' || ((node.kind === 'changeset' || node.kind === 'draft') && node.runId)) {
    links.push({ id: 'run', label: node.kind === 'run' ? 'Open run' : 'Open the run behind it' });
  }
  if (node.kind === 'run' || (node.kind === 'event' && node.id !== rootEventId)) {
    links.push({ id: 'thread', label: 'Open thread' });
  }
  if (node.gate) links.push({ id: 'review', label: 'Review changes' });
  return links;
}

/**
 * What a link kind opens for a node of the thread the HOST loaded. The webview sends a node id and a
 * kind; the target is read from the host's own node, so nothing a webview says can name a page, run
 * or skill. A kind the node does not offer, or a node that is not in the thread, opens nothing.
 */
export function resolveNodeLink(
  view: ThreadView,
  rootEventId: string,
  nodeId: string,
  link: NodeLinkKind,
): { command: string; args: unknown[] } | undefined {
  if (!NODE_LINK_KINDS.includes(link)) return undefined;
  const node = view.nodes.find((n) => n.id === nodeId);
  if (!node) return undefined;
  if (!nodeLinks(node, rootEventId).some((l) => l.id === link)) return undefined;
  switch (link) {
    case 'skill':
      return { command: 'escurel.viewSkill', args: [node.skill] };
    case 'page':
      return { command: 'escurel.openInstance', args: [node.pageId] };
    case 'run':
      return { command: 'escurel.openRun', args: [node.runId ?? node.id] };
    case 'thread':
      return node.kind === 'event'
        ? { command: 'escurel.openThread', args: [node.id] }
        : { command: 'escurel.openThread', args: [rootEventId] };
    case 'review':
      return commandForTarget(
        node.kind === 'changeset' || node.kind === 'draft' ? node.target : { open: 'nothing' },
      );
  }
}
