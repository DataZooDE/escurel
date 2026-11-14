/**
 * "Map Teams conversation" (#830): the `chat-channel` page an admin writes so a Teams group chat or
 * channel acts as an escurel group. The gateway validates the page too (`chat_channel_invalid`);
 * this mirrors its rules so the form can say what is wrong before anything is sent.
 */

/** The admin-only system skill a mapping is an instance of. */
export const CHAT_CHANNEL_SKILL = 'chat-channel';

export type MappingInput =
  | { kind: 'group'; conversationId: string; group: string; title?: string }
  | { kind: 'channel'; teamId: string; channelId: string; group: string; title?: string };

const RESERVED = new Set(['public', 'owner', 'admin']);

/** Why `group` cannot be a group principal, or undefined when it can (the gateway's rule). */
export function groupProblem(group: string): string | undefined {
  const g = group.trim();
  if (!g) return 'Name the escurel group the conversation acts as.';
  if (RESERVED.has(g) || g.startsWith('escurel:'))
    return `"${g}" is reserved; a conversation acts as an ordinary group.`;
  if (g.length > 64 || !/^[A-Za-z0-9._@-]+$/.test(g))
    return 'A group is 1-64 letters, digits, ".", "_", "-" or "@".';
  return undefined;
}

/** Why a Teams id is unusable, or undefined. */
export function idProblem(what: string, id: string): string | undefined {
  const v = id.trim();
  if (!v) return `Paste the ${what} (the bot's refusal in the chat names it).`;
  if (/[\s"\\]/.test(v)) return `A ${what} has no spaces or quotes.`;
  return undefined;
}

/** The first problem with the whole input, or undefined. */
export function mappingProblem(input: MappingInput): string | undefined {
  const ids =
    input.kind === 'group'
      ? [idProblem('conversation id', input.conversationId)]
      : [idProblem('team id', input.teamId), idProblem('channel id', input.channelId)];
  return ids.find(Boolean) ?? groupProblem(input.group);
}

/** A short stable hash, so one conversation always lands on one page. */
function shortHash(text: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h.toString(16).padStart(8, '0');
}

/** The page for `input`: `markdown/instances/chat-channel/teams-<group>-<hash>.md`. */
export function mappingPage(input: MappingInput): { pageId: string; content: string } {
  const problem = mappingProblem(input);
  if (problem) throw new Error(problem);
  const group = input.group.trim();
  const target =
    input.kind === 'group'
      ? `conversation_id: ${JSON.stringify(input.conversationId.trim())}`
      : `team_id: ${JSON.stringify(input.teamId.trim())}\nchannel_id: ${JSON.stringify(input.channelId.trim())}`;
  const key =
    input.kind === 'group'
      ? input.conversationId.trim()
      : `${input.teamId.trim()}/${input.channelId.trim()}`;
  const id = `teams-${group.toLowerCase().replace(/[^a-z0-9-]+/g, '-')}-${shortHash(key)}`;
  const title =
    input.title?.trim() ||
    (input.kind === 'group' ? `Teams group chat → ${group}` : `Teams channel → ${group}`);
  const content =
    `---\nkind: instance\nskill: ${CHAT_CHANNEL_SKILL}\nid: ${id}\ntitle: ${JSON.stringify(title)}\n` +
    `channel: msteams\n${target}\ngroup: ${group}\n---\n# ${title}\n\n` +
    `A member's @mention of the bot here acts as \`group:${group}\`, with ${group}'s access only.\n`;
  return { pageId: `markdown/instances/${CHAT_CHANNEL_SKILL}/${id}.md`, content };
}
