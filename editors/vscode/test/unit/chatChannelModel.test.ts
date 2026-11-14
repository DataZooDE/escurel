import { describe, expect, it } from 'vitest';
import {
  groupProblem,
  mappingPage,
  mappingProblem,
  mappingsOfTarget,
} from '../../src/views/chatChannelModel';

// The gateway's chat-channel rules (escurel-types::chat_channel): one target, a plain group.
describe('Map Teams conversation', () => {
  it('writes a group chat mapping the gateway accepts', () => {
    const page = mappingPage({
      kind: 'group',
      conversationId: ' 19:abc@thread.v2 ',
      group: 'sales',
    });
    expect(page.pageId).toMatch(/^markdown\/instances\/chat-channel\/teams-[0-9a-f]{8}\.md$/);
    expect(page.content).toContain('skill: chat-channel\n');
    expect(page.content).toContain('channel: msteams\n');
    expect(page.content).toContain('conversation_id: "19:abc@thread.v2"\n');
    expect(page.content).toContain('group: sales\n');
    expect(page.content).not.toContain('team_id');
    // One conversation, one page.
    expect(
      mappingPage({ kind: 'group', conversationId: '19:abc@thread.v2', group: 'sales' }).pageId,
    ).toBe(page.pageId);
  });

  it('writes a channel mapping by team and channel', () => {
    const page = mappingPage({
      kind: 'channel',
      teamId: '19:t@thread.tacv2',
      channelId: '19:c@thread.tacv2',
      group: 'ops',
    });
    expect(page.content).toContain(
      'team_id: "19:t@thread.tacv2"\nchannel_id: "19:c@thread.tacv2"\n',
    );
    expect(page.content).not.toContain('conversation_id');
  });

  it('refuses a reserved or malformed group and an empty id before anything is sent', () => {
    for (const bad of ['', 'admin', 'public', 'owner', 'escurel:admin', 'a b', 'x:y']) {
      expect(groupProblem(bad), bad).toBeTruthy();
    }
    expect(groupProblem('sales')).toBeUndefined();
    expect(mappingProblem({ kind: 'group', conversationId: ' ', group: 'sales' })).toMatch(
      /conversation id/,
    );
    expect(mappingProblem({ kind: 'channel', teamId: 't', channelId: '', group: 'sales' })).toMatch(
      /channel id/,
    );
    expect(() => mappingPage({ kind: 'group', conversationId: 'c', group: 'admin' })).toThrow(
      /reserved/,
    );
  });

  // Crew U-F2: remapping a chat to another group is the same page, and an existing page for the
  // target (by any id) is found before a second one is written.
  it('keys the page on the target, so a remap rewrites one page', () => {
    const sales = mappingPage({
      kind: 'group',
      conversationId: '19:abc@thread.v2',
      group: 'sales',
    });
    const ops = mappingPage({ kind: 'group', conversationId: '19:abc@thread.v2', group: 'ops' });
    expect(ops.pageId).toBe(sales.pageId);
    expect(ops.content).toContain('group: ops\n');
    const channel = mappingPage({ kind: 'channel', teamId: 't', channelId: 'c', group: 'sales' });
    expect(channel.pageId).not.toBe(sales.pageId);

    const existing = [
      {
        page_id: 'markdown/instances/chat-channel/hand-written.md',
        frontmatter: { channel: 'msteams', conversation_id: '19:abc@thread.v2', group: 'sales' },
      },
      {
        page_id: 'markdown/instances/chat-channel/deals.md',
        frontmatter: { channel: 'msteams', team_id: 't', channel_id: 'c', group: 'sales' },
      },
    ];
    const remap = { kind: 'group' as const, conversationId: ' 19:abc@thread.v2 ', group: 'ops' };
    expect(mappingsOfTarget(remap, existing).map((m) => m.page_id)).toEqual([
      'markdown/instances/chat-channel/hand-written.md',
    ]);
    expect(
      mappingsOfTarget({ kind: 'channel', teamId: 't', channelId: 'c', group: 'x' }, existing).map(
        (m) => m.page_id,
      ),
    ).toEqual(['markdown/instances/chat-channel/deals.md']);
    expect(
      mappingsOfTarget({ kind: 'group', conversationId: '19:other', group: 'x' }, existing),
    ).toEqual([]);
  });
});
