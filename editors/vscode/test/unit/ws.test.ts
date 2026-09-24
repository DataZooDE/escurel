import { afterEach, describe, expect, it } from 'vitest';
import { createServer, type IncomingMessage, type Server } from 'node:http';
import { WebSocketServer, type WebSocket } from 'ws';
import { EventSocket, type EventSocketOptions } from '../../src/client/ws';

// A fake /ws that speaks the gateway's frames (crates/escurel-server/src/ws.rs):
// Bearer on the upgrade, `hello {presence_only}`, one `event_subscribe` per
// socket answered with an ack, `event` pushes (with `replayed` on catch-up),
// `event_lagged`, and the plain-HTTP refusals on the upgrade (401 / 429).
interface Upgrade {
  authorization?: string;
  frames: Record<string, unknown>[];
  socket: WebSocket;
}

async function startFakeWs(
  refuse: (n: number) => { status: number; body: string } | undefined = () => undefined,
) {
  const upgrades: Upgrade[] = [];
  const server: Server = createServer((_req, res) => res.writeHead(404).end());
  const wss = new WebSocketServer({ noServer: true });
  server.on('upgrade', (req: IncomingMessage, socket, head) => {
    const r = refuse(upgrades.length);
    if (r) {
      socket.write(
        `HTTP/1.1 ${r.status} X\r\ncontent-type: application/json\r\ncontent-length: ${Buffer.byteLength(r.body)}\r\nconnection: close\r\n\r\n${r.body}`,
      );
      socket.destroy();
      upgrades.push({
        authorization: req.headers.authorization,
        frames: [],
        socket: undefined as unknown as WebSocket,
      });
      return;
    }
    wss.handleUpgrade(req, socket, head, (ws) => {
      const up: Upgrade = { authorization: req.headers.authorization, frames: [], socket: ws };
      upgrades.push(up);
      ws.on('message', (data) => {
        const frame = JSON.parse(String(data));
        up.frames.push(frame);
        if (frame.type === 'event_subscribe')
          ws.send(
            JSON.stringify({ type: 'event_subscribe_ack', subscription_id: frame.subscription_id }),
          );
      });
    });
  });
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  const { port } = server.address() as { port: number };
  const waitFor = async (pred: () => boolean, ms = 3000) => {
    const end = Date.now() + ms;
    while (!pred()) {
      if (Date.now() > end) throw new Error('timed out waiting');
      await new Promise((r) => setTimeout(r, 10));
    }
  };
  return {
    url: `http://127.0.0.1:${port}`,
    upgrades,
    waitFor,
    push: (i: number, event: Record<string, unknown>, replayed = false) =>
      upgrades[i]!.socket.send(
        JSON.stringify({
          type: 'event',
          subscription_id: upgrades[i]!.frames.find((f) => f.type === 'event_subscribe')
            ?.subscription_id,
          event,
          ...(replayed ? { replayed: true } : {}),
        }),
      ),
    close: async () => {
      for (const u of upgrades) u.socket?.close();
      wss.close();
      await new Promise<void>((r) => server.close(() => r()));
    },
  };
}

function options(
  url: string,
  extra: Partial<EventSocketOptions> = {},
): EventSocketOptions & { events: { id: string; replayed: boolean }[]; warnings: string[] } {
  const events: { id: string; replayed: boolean }[] = [];
  const warnings: string[] = [];
  return {
    gatewayUrl: url,
    tokens: { get: async () => 'tok-1' },
    filters: { root_event_id: 'root-1' },
    reconnect: { minMs: 20, maxMs: 40 },
    onEvent: (e, replayed) => events.push({ id: e.event_id, replayed }),
    onWarning: (kind) => warnings.push(kind),
    onError: () => {},
    events,
    warnings,
    ...extra,
  };
}

const sockets: EventSocket[] = [];
const servers: { close: () => Promise<void> }[] = [];
afterEach(async () => {
  for (const s of sockets.splice(0)) s.close();
  for (const s of servers.splice(0)) await s.close();
});

describe('EventSocket', () => {
  it('sends the bearer on the upgrade, hello presence_only, then one event_subscribe with the filters', async () => {
    const srv = await startFakeWs();
    servers.push(srv);
    const o = options(srv.url, { sinceEventId: 'e-0' });
    const s = new EventSocket(o);
    sockets.push(s);
    s.connect();
    await srv.waitFor(() => srv.upgrades[0]?.frames.length === 2);
    const up = srv.upgrades[0]!;
    expect(up.authorization).toBe('Bearer tok-1');
    expect(up.frames[0]).toEqual({ type: 'hello', presence_only: true });
    expect(up.frames[1]).toMatchObject({
      type: 'event_subscribe',
      since_event_id: 'e-0',
      filters: { root_event_id: 'root-1' },
    });
  });

  it('delivers events with the replayed flag and drops duplicates by event_id', async () => {
    const srv = await startFakeWs();
    servers.push(srv);
    const o = options(srv.url);
    const s = new EventSocket(o);
    sockets.push(s);
    s.connect();
    await srv.waitFor(() => srv.upgrades[0]?.frames.length === 2);
    srv.push(0, { event_id: 'e-1' }, true);
    srv.push(0, { event_id: 'e-1' });
    srv.push(0, { event_id: 'e-2' });
    await srv.waitFor(() => o.events.length === 2);
    expect(o.events).toEqual([
      { id: 'e-1', replayed: true },
      { id: 'e-2', replayed: false },
    ]);
    expect(s.lastEventId).toBe('e-2');
  });

  it('resumes after a drop with since_event_id = the last event seen', async () => {
    const srv = await startFakeWs();
    servers.push(srv);
    const o = options(srv.url);
    const s = new EventSocket(o);
    sockets.push(s);
    s.connect();
    await srv.waitFor(() => srv.upgrades[0]?.frames.length === 2);
    srv.push(0, { event_id: 'e-7' });
    await srv.waitFor(() => o.events.length === 1);
    srv.upgrades[0]!.socket.close();
    await srv.waitFor(() => srv.upgrades[1]?.frames.length === 2);
    expect(srv.upgrades[1]!.frames[1]).toMatchObject({
      type: 'event_subscribe',
      since_event_id: 'e-7',
    });
  });

  it('surfaces event_lagged as a warning (the view reconciles by polling)', async () => {
    const srv = await startFakeWs();
    servers.push(srv);
    const o = options(srv.url);
    const s = new EventSocket(o);
    sockets.push(s);
    s.connect();
    await srv.waitFor(() => srv.upgrades[0]?.frames.length === 2);
    srv.upgrades[0]!.socket.send(
      JSON.stringify({
        type: 'event_lagged',
        subscription_id: 1,
        skipped: 3,
        message: 'fell behind',
      }),
    );
    await srv.waitFor(() => o.warnings.length === 1);
    expect(o.warnings).toEqual(['lagged']);
  });

  it('a 429 session_cap_reached on the upgrade is a warning and stops reconnecting', async () => {
    const srv = await startFakeWs(() => ({
      status: 429,
      body: '{"error":"session_cap_reached","message":"cap"}',
    }));
    servers.push(srv);
    const o = options(srv.url);
    const s = new EventSocket(o);
    sockets.push(s);
    s.connect();
    await srv.waitFor(() => o.warnings.length === 1);
    expect(o.warnings).toEqual(['session_cap_reached']);
    await new Promise((r) => setTimeout(r, 150));
    expect(srv.upgrades.length).toBe(1);
    expect(s.state).toBe('capped');
  });

  it('a 401 on the upgrade reports unauthorized and stops', async () => {
    const srv = await startFakeWs(() => ({
      status: 401,
      body: '{"error":"unauthorized","message":"token rejected"}',
    }));
    servers.push(srv);
    const errors: string[] = [];
    const o = options(srv.url, { onError: (e) => errors.push(e.kind) });
    const s = new EventSocket(o);
    sockets.push(s);
    s.connect();
    await srv.waitFor(() => errors.length === 1);
    expect(errors).toEqual(['unauthorized']);
    expect(s.state).toBe('closed');
  });

  it('reconnects with the new bearer when the token source refreshes', async () => {
    const srv = await startFakeWs();
    servers.push(srv);
    let token = 'tok-1';
    let listener: ((t: string | undefined) => void) | undefined;
    const o = options(srv.url, {
      tokens: { get: async () => token, onDidRefresh: (l) => ((listener = l), { dispose() {} }) },
    });
    const s = new EventSocket(o);
    sockets.push(s);
    s.connect();
    await srv.waitFor(() => srv.upgrades[0]?.frames.length === 2);
    token = 'tok-2';
    listener?.(token);
    await srv.waitFor(() => srv.upgrades[1]?.frames.length === 2);
    expect(srv.upgrades[1]!.authorization).toBe('Bearer tok-2');
  });

  it('sends no Authorization header without a token (dev gateway)', async () => {
    const srv = await startFakeWs();
    servers.push(srv);
    const s = new EventSocket(options(srv.url, { tokens: { get: async () => undefined } }));
    sockets.push(s);
    s.connect();
    await srv.waitFor(() => srv.upgrades[0]?.frames.length === 2);
    expect(srv.upgrades[0]!.authorization).toBeUndefined();
  });
});
