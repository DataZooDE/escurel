// Conversations are instances of the `conversation` skill (the chat corpus): one owner, one
// page-bound chat. Recent = the owner's instances, newest first; New = a page created with
// `base_sha256: ""` and the signed-in subject as `owner`. The gateway's ACL keeps them private.
import { randomBytes } from 'node:crypto';
import type { EscurelClient, Instance } from '../client';
import { cleanText } from '../shared/untrustedText';

export const CONVERSATION_SKILL = 'conversation';

export interface Conversation {
  pageId: string;
  title: string;
  startedAt: string;
}

const RECENT_LIMIT = 30;

function toConversation(i: Instance): Conversation {
  const fm = i.frontmatter;
  const title = typeof fm.title === 'string' ? fm.title : '';
  const startedAt = typeof fm.started_at === 'string' ? fm.started_at : (i.at ?? '');
  return { pageId: i.page_id, title: cleanText(title || 'Untitled chat', 80), startedAt };
}

/** The signed-in person's conversations, newest first (the gateway returns only theirs). */
export async function recentConversations(client: EscurelClient): Promise<Conversation[]> {
  const page = await client.listInstancesPage({
    skill_id: CONVERSATION_SKILL,
    order_by: 'at desc',
    limit: RECENT_LIMIT,
  });
  return page.instances.map(toConversation);
}

/** A page id the gateway accepts (`[A-Za-z0-9._-]`), unique without a round trip. */
export function newConversationId(now: Date = new Date()): string {
  const stamp = now
    .toISOString()
    .replace(/[-:TZ.]/g, '')
    .slice(0, 14);
  return `c-${stamp}-${randomBytes(3).toString('hex')}`;
}

/** YAML double-quoted scalar: the title is the person's words, the subject is from the token. */
const quoted = (s: string) => JSON.stringify(s);

/** Create a conversation titled after its first question; returns it. */
export async function createConversation(
  client: EscurelClient,
  owner: string,
  firstQuestion: string,
  now: Date = new Date(),
): Promise<Conversation> {
  const id = newConversationId(now);
  const pageId = `markdown/instances/${CONVERSATION_SKILL}/${id}.md`;
  const title = cleanText(firstQuestion, 60) || 'New chat';
  const startedAt = now.toISOString().replace(/\.\d{3}Z$/, 'Z');
  const content =
    `---\nkind: instance\nskill: ${CONVERSATION_SKILL}\nid: ${id}\ntitle: ${quoted(title)}\n` +
    `owner: ${quoted(owner)}\nstarted_at: ${startedAt}\n---\n# ${title}\n`;
  await client.updatePage({ page_id: pageId, content, base_sha256: '' });
  return { pageId, title, startedAt };
}
