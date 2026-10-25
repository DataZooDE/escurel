import { cleanText } from '../shared/untrustedText';
import type { OverviewTile, OverviewTone } from '../shared/protocol';
import type { OverviewAction } from '../overview/model';
import type { GatewayClient, OverviewTileContribution, PageRenderer } from './types';

interface Disposable {
  dispose(): void;
}

const TILE_ID = /^[a-z0-9][a-z0-9._-]{0,39}$/;
const COMMAND_ID = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/;
/** A contribution that does not answer within this is shown as unavailable; it never holds the board. */
export const TILE_TIMEOUT_MS = 5_000;
const MAX_LINES = 5;
const TONES: readonly OverviewTone[] = ['attention', 'ok', 'neutral'];

export const contributedTileId = (id: string): `ext:${string}` => `ext:${id}`;

/** Who registered what, with a change signal. One implementation for both registries. */
class Registry<V> {
  private readonly entries = new Map<string, V>();
  private readonly listeners = new Set<() => void>();

  get size(): number {
    return this.entries.size;
  }

  get(key: string): V | undefined {
    return this.entries.get(key);
  }

  values(): V[] {
    return [...this.entries.values()];
  }

  onDidChange(listener: () => void): Disposable {
    this.listeners.add(listener);
    return { dispose: () => void this.listeners.delete(listener) };
  }

  protected add(key: string, value: V): Disposable {
    if (this.entries.has(key)) throw new Error(`already registered: ${key}`);
    this.entries.set(key, value);
    this.changed();
    return {
      dispose: () => {
        if (this.entries.get(key) === value) {
          this.entries.delete(key);
          this.changed();
        }
      },
    };
  }

  private changed(): void {
    for (const l of [...this.listeners]) l();
  }
}

export class OverviewTileRegistry extends Registry<OverviewTileContribution> {
  register(tile: OverviewTileContribution): Disposable {
    if (!tile || typeof tile.id !== 'string' || !TILE_ID.test(tile.id)) {
      throw new Error('registerOverviewTile: invalid id');
    }
    if (typeof tile.title !== 'string' || !cleanText(tile.title, 80)) {
      throw new Error('registerOverviewTile: a tile needs a title');
    }
    if (typeof tile.load !== 'function') throw new Error('registerOverviewTile: load is missing');
    if (tile.openCommand !== undefined && !COMMAND_ID.test(tile.openCommand)) {
      throw new Error('registerOverviewTile: invalid openCommand');
    }
    return this.add(tile.id, tile);
  }
}

export class PageRendererRegistry extends Registry<PageRenderer> {
  register(skill: string, renderer: PageRenderer): Disposable {
    if (typeof skill !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/.test(skill)) {
      throw new Error('registerPageRenderer: invalid skill id');
    }
    if (!renderer || typeof renderer.render !== 'function') {
      throw new Error('registerPageRenderer: render is missing');
    }
    return this.add(skill, renderer);
  }
}

export interface ContributedTiles {
  tiles: OverviewTile[];
  actions: Map<string, OverviewAction>;
  /** Tile id -> the command its title opens. */
  openCommands: Map<string, string>;
}

const tone = (t: unknown, fallback: OverviewTone): OverviewTone =>
  TONES.includes(t as OverviewTone) ? (t as OverviewTone) : fallback;

function withTimeout<T>(run: (signal: AbortSignal) => Promise<T>, ms: number): Promise<T> {
  const ac = new AbortController();
  let timer: NodeJS.Timeout | undefined;
  const timeout = new Promise<never>((_, reject) => {
    timer = setTimeout(() => {
      ac.abort();
      reject(new Error('timed out'));
    }, ms);
  });
  return Promise.race([run(ac.signal), timeout]).finally(() => clearTimeout(timer));
}

/**
 * Reads every registered tile and turns the answers into board tiles. What a provider returns is
 * data from another extension, so it is cleaned and bounded like gateway text; commands stay in the
 * host's action table and a line carries only its key. One failing provider yields an "unavailable"
 * tile, never an empty board.
 */
export async function loadContributedTiles(
  registry: OverviewTileRegistry,
  client: GatewayClient,
  timeoutMs = TILE_TIMEOUT_MS,
): Promise<ContributedTiles> {
  const out: ContributedTiles = { tiles: [], actions: new Map(), openCommands: new Map() };
  const results = await Promise.all(
    registry.values().map(async (c) => {
      try {
        return { c, data: await withTimeout((signal) => c.load({ client, signal }), timeoutMs) };
      } catch {
        return { c, data: undefined };
      }
    }),
  );
  for (const { c, data } of results) {
    const id = contributedTileId(c.id);
    const title = cleanText(c.title, 80);
    if (c.openCommand) out.openCommands.set(id, c.openCommand);
    if (!data || typeof data !== 'object' || typeof data.headline !== 'string') {
      out.tiles.push({
        id,
        title,
        headline: 'Not available right now',
        tone: 'attention',
        items: [],
        empty: 'This tile could not be read.',
      });
      continue;
    }
    const lines = Array.isArray(data.items) ? data.items : [];
    const items = lines
      .filter((l) => l && typeof l.label === 'string')
      .slice(0, MAX_LINES)
      .map((l, i) => {
        const key = `${id}:${i}`;
        if (typeof l.command === 'string' && COMMAND_ID.test(l.command)) {
          out.actions.set(key, { command: l.command, args: Array.isArray(l.args) ? l.args : [] });
        }
        return {
          key,
          label: cleanText(l.label, 120),
          ...(typeof l.detail === 'string' ? { detail: cleanText(l.detail, 160) } : {}),
          tone: tone(l.tone, 'neutral'),
        };
      });
    const extra = Math.max(0, lines.length - MAX_LINES);
    const more = extra + (Number.isFinite(data.more) ? Math.max(0, Math.trunc(data.more!)) : 0);
    out.tiles.push({
      id,
      title,
      headline: cleanText(data.headline, 120),
      tone: tone(data.tone, 'neutral'),
      items,
      ...(more > 0 ? { more } : {}),
      empty: cleanText(typeof data.empty === 'string' ? data.empty : 'Nothing to show.', 120),
    });
  }
  return out;
}
