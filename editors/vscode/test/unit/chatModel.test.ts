import { createHash } from 'node:crypto';
import { describe, expect, it } from 'vitest';
import type { ChatMessage } from '../../src/client';
import { chatGroupId, foldChat, wikilinksIn } from '../../src/chat/chatModel';

const PAGE = 'markdown/instances/conversation/c-1.md';

const user = (msg_id: string, ts: string, content: string): ChatMessage => ({
  chat_group_id: 'g',
  msg_id,
  ts,
  role: 'user',
  content,
  embedded: false,
  author: 'alice',
});
const reply = (
  runId: string,
  ts: string,
  content: string,
  userMsgId: string,
  extra: Record<string, unknown> = {},
): ChatMessage => ({
  chat_group_id: 'g',
  msg_id: `run:${runId}:reply`,
  ts,
  role: 'assistant',
  content,
  embedded: true,
  author: 'alice',
  metadata: {
    status: 'ok',
    drafts: [],
    cited: [],
    user_msg_id: userMsgId,
    run_id: runId,
    ...extra,
  },
});

describe('chatGroupId', () => {
  it('is the gateway page-chat id: the owner subject hashed, then the page', () => {
    const hash = createHash('sha256').update('alice').digest('hex');
    expect(chatGroupId('alice', PAGE)).toBe(`page-chat:v1:${hash}:${PAGE}`);
  });
});

describe('foldChat', () => {
  it('pairs each answer with its question, in order, and keeps the run for "Show how I got this"', () => {
    const rows = foldChat(
      [
        reply('R1', '2026-10-25T10:00:05Z', 'Globex renews. [[customer::globex]]', 'u-1'),
        user('u-1', '2026-10-25T10:00:00Z', 'Which customers renew?'),
      ],
      {},
    );
    expect(rows.map((r) => r.kind)).toEqual(['user', 'assistant']);
    const answer = rows[1]!;
    expect(answer.kind === 'assistant' && answer.runId).toBe('R1');
    expect(answer.kind === 'assistant' && answer.status).toBe('ok');
  });

  it('shows a working line for a question with no answer yet, and the progress step when known', () => {
    const msgs = [user('u-1', '2026-10-25T10:00:00Z', 'Which customers renew?')];
    expect(foldChat(msgs, {}).at(-1)).toMatchObject({ kind: 'pending', text: 'Working on it…' });
    expect(foldChat(msgs, { progress: 'searching' }).at(-1)).toMatchObject({
      kind: 'pending',
      text: 'Working on it: searching',
    });
  });

  it('says plainly that no agent is running, instead of spinning forever', () => {
    const rows = foldChat([user('u-1', '2026-10-25T10:00:00Z', 'Hello?')], {
      runnerAlive: false,
    });
    expect(rows.at(-1)).toMatchObject({
      kind: 'notice',
      text: 'No agent is running. Your question is saved and will be answered when one starts.',
    });
  });

  it('answers "yes" twice as two turns, and drops a duplicate delivery of the same message', () => {
    const rows = foldChat(
      [
        user('u-1', '2026-10-25T10:00:00Z', 'yes'),
        reply('R1', '2026-10-25T10:00:03Z', 'echo: yes', 'u-1'),
        user('u-2', '2026-10-25T10:01:00Z', 'yes'),
        user('u-2', '2026-10-25T10:01:00Z', 'yes'),
        reply('R2', '2026-10-25T10:01:03Z', 'echo: yes', 'u-2'),
      ],
      {},
    );
    expect(rows.map((r) => r.kind)).toEqual(['user', 'assistant', 'user', 'assistant']);
  });

  it('marks a failed answer and keeps its drafts', () => {
    const rows = foldChat(
      [
        user('u-1', '2026-10-25T10:00:00Z', 'Add a note'),
        reply('R1', '2026-10-25T10:00:03Z', "I couldn't finish: the agent run failed.", 'u-1', {
          status: 'failed',
          drafts: ['D1'],
        }),
      ],
      {},
    );
    expect(rows[1]).toMatchObject({ kind: 'assistant', status: 'failed', drafts: ['D1'] });
  });

  it('cleans what it shows: no control or bidi characters reach the view', () => {
    const rows = foldChat(
      [
        user(
          'u-1',
          '2026-10-25T10:00:00Z',
          `a${String.fromCodePoint(0x202e)}b${String.fromCodePoint(7)}c`,
        ),
      ],
      {},
    );
    expect(rows[0]).toMatchObject({ kind: 'user', text: 'abc' });
  });
});

describe('wikilinksIn', () => {
  it('lists the [[skill::id]] targets an answer cites, once each', () => {
    expect(
      wikilinksIn('See [[customer::globex]] and [[customer::globex|Globex]], [[deal::d-1]].'),
    ).toEqual(['customer::globex', 'deal::d-1']);
  });
});
