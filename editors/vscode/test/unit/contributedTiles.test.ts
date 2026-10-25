import { describe, expect, it } from 'vitest';
import { OverviewTileRegistry, loadContributedTiles } from '../../src/api/contributions';
import type { GatewayClient } from '../../src/api/types';

const client: GatewayClient = { callTool: () => Promise.reject(new Error('unused')) };

describe('loadContributedTiles', () => {
  it('turns provider data into a tile; commands stay in the host table', async () => {
    const reg = new OverviewTileRegistry();
    reg.register({
      id: 'acme.example',
      title: 'Example',
      openCommand: 'acme.example.open',
      load: async () => ({
        headline: 'Three things',
        tone: 'attention',
        items: [
          { label: 'one‮', detail: 'd', tone: 'ok', command: 'acme.example.show', args: [1] },
          { label: 'no command' },
        ],
      }),
    });
    const out = await loadContributedTiles(reg, client);
    const [tile] = out.tiles;
    expect(tile).toMatchObject({ id: 'ext:acme.example', title: 'Example', tone: 'attention' });
    expect(tile!.items.map((i) => i.key)).toEqual(['ext:acme.example:0', 'ext:acme.example:1']);
    expect(tile!.items[0]!.label).toBe('one');
    expect(JSON.stringify(tile)).not.toContain('acme.example.show');
    expect(out.actions.get('ext:acme.example:0')).toEqual({
      command: 'acme.example.show',
      args: [1],
    });
    expect(out.actions.has('ext:acme.example:1')).toBe(false);
    expect(out.openCommands.get('ext:acme.example')).toBe('acme.example.open');
  });

  it('caps the lines and counts the rest', async () => {
    const reg = new OverviewTileRegistry();
    reg.register({
      id: 'many',
      title: 'Many',
      load: async () => ({
        headline: 'h',
        items: Array.from({ length: 8 }, (_, i) => ({ label: `l${i}` })),
        more: 4,
      }),
    });
    const [tile] = (await loadContributedTiles(reg, client)).tiles;
    expect(tile!.items).toHaveLength(5);
    expect(tile!.more).toBe(7);
  });

  it('refuses a command id that is not a command id', async () => {
    const reg = new OverviewTileRegistry();
    reg.register({
      id: 'x',
      title: 'X',
      load: async () => ({ headline: 'h', items: [{ label: 'a', command: 'a b; c' }] }),
    });
    expect((await loadContributedTiles(reg, client)).actions.size).toBe(0);
  });

  it('shows an unavailable tile for a provider that throws or hangs, and keeps the others', async () => {
    const reg = new OverviewTileRegistry();
    reg.register({ id: 'bad', title: 'Bad', load: () => Promise.reject(new Error('boom')) });
    reg.register({ id: 'slow', title: 'Slow', load: () => new Promise(() => undefined) });
    reg.register({ id: 'good', title: 'Good', load: async () => ({ headline: 'fine' }) });
    const out = await loadContributedTiles(reg, client, 20);
    const by = Object.fromEntries(out.tiles.map((t) => [t.id, t]));
    expect(by['ext:bad']!.headline).toBe('Not available right now');
    expect(by['ext:slow']!.tone).toBe('attention');
    expect(by['ext:good']!.headline).toBe('fine');
  });
});
